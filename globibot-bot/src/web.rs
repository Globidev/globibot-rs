use axum::{
    BoxError, Json, Router,
    extract::{FromRef, State},
    http::StatusCode,
    response::{self, IntoResponse, Redirect, Response, Sse, sse::Event},
    routing::{get, post},
};
use axum_extra::extract::{
    PrivateCookieJar,
    cookie::{Cookie, Key},
};
use futures::{Stream, TryStreamExt};
use globibot_core::{
    serenity::{self, Error, all::GuildId},
    storage::{DiscordSession, RedisStorage, StorageValue},
};
use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};
use tokio::{net::ToSocketAddrs, sync::broadcast::Receiver};

#[derive(Debug, Clone, FromRef)]
pub struct WebServer {
    storage: RedisStorage,
    cookie_key: Key,

    #[from_ref(skip)]
    discord_app_id: u64,
    #[from_ref(skip)]
    discord_app_secret: String,
    #[from_ref(skip)]
    discord_oauth_authorize_link: String,
    #[from_ref(skip)]
    discord_guild_id: GuildId,
}

impl WebServer {
    pub fn new(
        storage: RedisStorage,
        cookie_secret: &str,
        discord_app_id: u64,
        discord_app_secret: String,
        discord_oauth_authorize_link: String,
        discord_guild_id: GuildId,
    ) -> Self {
        Self {
            storage,
            cookie_key: Key::from(cookie_secret.as_bytes()),
            discord_app_id,
            discord_app_secret,
            discord_oauth_authorize_link,
            discord_guild_id,
        }
    }

    pub async fn serve(self, addr: impl ToSocketAddrs) -> std::io::Result<()> {
        let app = Router::new()
            .route("/", get(async || "Globibot Web Server"))
            .route("/plugins", get(list_plugins))
            .route("/sse", get(stream_events))
            .with_state(SseMessageReceiver {
                rx: WEB_STATE.lock().unwrap().tx.subscribe(),
            })
            .route("/discord/authorize", get(discord_authorize))
            .route("/discord/login", post(discord_login))
            .route("/discord/logout", post(discord_logout))
            .route("/discord/profile", get(discord_profile))
            .with_state(self);

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }
}

async fn list_plugins() -> Json<Vec<ConnectedPlugin>> {
    let plugins = WEB_STATE
        .lock()
        .unwrap()
        .plugins
        .values()
        .cloned()
        .collect();
    Json(plugins)
}

async fn stream_events(
    State(SseMessageReceiver { rx }): State<SseMessageReceiver>,
) -> Sse<impl Stream<Item = Result<Event, BoxError>>> {
    use tokio_stream::wrappers::BroadcastStream;

    let event_stream = BroadcastStream::new(rx)
        .err_into()
        .and_then(|msg| async { Ok(Event::default().json_data(msg)?) });
    Sse::new(event_stream)
}

async fn discord_authorize(State(web_server): State<WebServer>) -> impl IntoResponse {
    Redirect::temporary(&web_server.discord_oauth_authorize_link)
}

#[derive(serde::Deserialize)]
struct OAuthCallbackParams {
    code: String,
}

#[derive(Debug, thiserror::Error)]
enum OauthCallbackError {
    #[error("HTTP request error: {0}")]
    Reqwest(#[from] reqwest::Error),

    #[error("Storage error: {0}")]
    Storage(#[from] globibot_core::storage::StorageError),
}

impl IntoResponse for OauthCallbackError {
    fn into_response(self) -> response::Response {
        let error_msg = format!("OAuth callback error: {self}");
        (StatusCode::INTERNAL_SERVER_ERROR, error_msg).into_response()
    }
}

async fn discord_login(
    State(mut web_server): State<WebServer>,
    jar: PrivateCookieJar,
    Json(params): Json<OAuthCallbackParams>,
) -> Result<impl IntoResponse, OauthCallbackError> {
    let now = time::OffsetDateTime::now_utc();

    let client = reqwest::Client::new();

    let authorize_url = url::Url::parse(&web_server.discord_oauth_authorize_link)
        .expect("Invalid Discord OAuth authorize link");
    let redirect_uri = authorize_url
        .query_pairs()
        .find_map(|(k, v)| (k == "redirect_uri").then_some(v))
        .expect("redirect_uri not found in Discord OAuth authorize link");

    let params = [
        ("grant_type", "authorization_code"),
        ("code", &params.code),
        ("redirect_uri", &redirect_uri),
    ];

    #[derive(serde::Deserialize)]
    struct DiscordToken {
        access_token: String,
        expires_in: i64,
        refresh_token: String,
    }

    tracing::info!("Exchanging OAuth code for token...");

    let DiscordToken {
        access_token,
        expires_in,
        refresh_token,
    } = client
        .post("https://discord.com/api/v10/oauth2/token")
        .basic_auth(
            web_server.discord_app_id,
            Some(&web_server.discord_app_secret),
        )
        .form(&params)
        .send()
        .await?
        .error_for_status()?
        .json()
        .await?;

    tracing::info!("OAuth token received, storing session...");

    let session_id = uuid::Uuid::new_v4();

    let expiry_date = now + time::Duration::seconds(expires_in);
    let expires_at_ts = expiry_date.unix_timestamp();

    let discord_session = DiscordSession {
        access_token,
        refresh_token,
        expires_at_ts,
    };
    web_server
        .storage
        .set(&session_id, &discord_session)
        .await?;

    tracing::info!("Session stored, setting cookie...");

    let jar = jar.add(
        Cookie::build(("discord_session", session_id.to_string()))
            .path("/")
            .http_only(true)
            .secure(true)
            .expires(expiry_date), // 🔶 Might not be needed when supporting automatic token refreshes
    );

    Ok(discord_profile(State(web_server), jar).await)
}

async fn discord_logout(
    State(mut web_server): State<WebServer>,
    jar: PrivateCookieJar,
) -> impl IntoResponse {
    let Some(cookie) = jar.get("discord_session") else {
        return (StatusCode::BAD_REQUEST, "No session").into_response();
    };

    match web_server
        .storage
        .del::<DiscordSession>(cookie.value())
        .await
    {
        Ok(_) => {
            let jar = jar.remove(
                Cookie::build("discord_session")
                    .path("/")
                    .http_only(true)
                    .secure(true),
            );
            (jar, "Logged out").into_response()
        }
        Err(err) => {
            tracing::warn!("Failed to delete Discord session from storage: {err}");
            (StatusCode::INTERNAL_SERVER_ERROR, "Failed to log out").into_response()
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
struct DiscordProfile {
    username: String,
    avatar_url: Option<String>,
}

impl StorageValue for DiscordProfile {
    const REDIS_NS: &'static str = "discord_profile";
}

async fn discord_profile(State(mut server): State<WebServer>, jar: PrivateCookieJar) -> Response {
    tracing::info!("Fetching Discord profile...");

    let Some(cookie) = jar.get("discord_session") else {
        return (StatusCode::BAD_REQUEST, "No session").into_response();
    };

    let session_id = cookie.value();

    macro_rules! bad_request {
        ($text:expr) => {
            return (
                StatusCode::BAD_REQUEST,
                jar.remove(
                    Cookie::build("discord_session")
                        .path("/")
                        .http_only(true)
                        .secure(true),
                ),
                $text,
            )
                .into_response()
        };
    }

    tracing::info!("Retrieving profile from cache...");

    match server.storage.get::<DiscordProfile>(session_id).await {
        Ok(profile) => return (jar, Json(profile)).into_response(),
        Err(_) => {
            tracing::info!("No cached profile found, fetching from Discord API...");
        }
    };

    tracing::info!("Retrieving session from storage...");

    let session = match server.storage.get::<DiscordSession>(session_id).await {
        Ok(session) => session,
        Err(err) => {
            tracing::warn!("Failed to get Discord session from storage: {err}");
            bad_request!("Invalid session");
        }
    };

    tracing::info!("Session retrieved, fetching user profile from Discord...");

    let http = serenity::http::Http::new(&format!("Bearer {}", session.access_token));

    let member = match http
        .get_current_user_guild_member(server.discord_guild_id)
        .await
    {
        Ok(user) => user,
        Err(Error::Http(err))
            if err
                .status_code()
                .is_some_and(|status| status.as_u16() == 404) =>
        {
            bad_request!("You are not a member of the required Discord guild");
        }
        Err(err) => {
            bad_request!(format!("Failed to get user: {err}"));
        }
    };

    tracing::info!("User profile fetched successfully.");

    let profile = DiscordProfile {
        username: member.display_name().to_string(),
        avatar_url: member.avatar_url().or(member.user.avatar_url()),
    };

    if let Err(err) = server.storage.set(session_id, &profile).await {
        tracing::warn!("Failed to cache Discord profile: {err}");
    }
    if let Err(err) = server
        .storage
        .expire::<DiscordProfile>(session_id, 3_600)
        .await
    {
        tracing::warn!("Failed to set expiration for Discord profile: {err}");
    }

    (jar, Json(profile)).into_response()
}

#[derive(Debug, Clone, serde::Serialize)]
enum SseMessage {
    UpsertedPlugin(ConnectedPlugin),
    RemovedPlugin(String),
}

#[derive(Debug, Clone, serde::Serialize)]
struct ConnectedPlugin {
    name: String,
    has_rpc: bool,
    has_events: bool,
    has_web_api: bool,
    startup_ts: u64,
}

struct SseMessageReceiver {
    rx: Receiver<SseMessage>,
}

impl Clone for SseMessageReceiver {
    fn clone(&self) -> Self {
        Self {
            rx: self.rx.resubscribe(),
        }
    }
}

pub static WEB_STATE: LazyLock<Mutex<WebServerState>> = LazyLock::new(|| {
    Mutex::new(WebServerState {
        plugins: HashMap::new(),
        tx: tokio::sync::broadcast::channel(1 << 8).0,
    })
});

#[derive(Debug)]
pub struct WebServerState {
    plugins: HashMap<String, ConnectedPlugin>,
    tx: tokio::sync::broadcast::Sender<SseMessage>,
}

impl WebServerState {
    pub fn register_plugin_rpc(&mut self, name: &str) {
        let plugin = self.get_or_create_plugin(name);
        plugin.has_rpc = true;

        let plugin = plugin.clone();
        self.tx.send(SseMessage::UpsertedPlugin(plugin)).ok();
    }

    pub fn register_plugin_events(&mut self, name: &str) {
        let existing_plugin = self.get_or_create_plugin(name);
        existing_plugin.has_events = true;

        let plugin = existing_plugin.clone();
        self.tx.send(SseMessage::UpsertedPlugin(plugin)).ok();
    }

    pub fn register_plugin_web_api(&mut self, name: &str) {
        let existing_plugin = self.get_or_create_plugin(name);
        existing_plugin.has_web_api = true;

        let plugin = existing_plugin.clone();
        self.tx.send(SseMessage::UpsertedPlugin(plugin)).ok();
    }

    fn get_or_create_plugin(&mut self, name: &str) -> &mut ConnectedPlugin {
        self.plugins
            .entry(name.to_string())
            .or_insert_with(|| ConnectedPlugin {
                name: name.to_string(),
                has_rpc: false,
                has_events: false,
                has_web_api: false,
                startup_ts: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            })
    }

    pub fn unregister_plugin_rpc(&mut self, name: &str) {
        let Some(_plugin) = self.plugins.remove(name) else {
            return;
        };
        let message = SseMessage::RemovedPlugin(name.to_string());
        self.tx.send(message).ok();
    }
}
