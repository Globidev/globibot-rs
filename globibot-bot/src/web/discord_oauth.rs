use std::sync::Arc;

use axum::{
    Json,
    extract::{Query, State},
    http::StatusCode,
    response::{self, IntoResponse, Response},
};
use axum_extra::extract::{
    PrivateCookieJar,
    cookie::{Cookie, SameSite},
};
use globibot_core::{
    serenity,
    storage::{DiscordProfile, DiscordSession},
};

use crate::web::WebServer;

pub fn router() -> axum::Router<WebServer> {
    use axum::routing::{get, post};

    axum::Router::new()
        .route("/authorize", get(authorize))
        .route("/login", post(login))
        .route("/logout", post(logout))
        .route("/profile", get(profile))
}

#[derive(Debug)]
pub struct Config {
    pub app_id: u64,
    pub app_secret: String,
    pub guild_id: globibot_core::serenity::all::GuildId,
    pub oauth_authorize_link: String,
}

/// Holds the `state` sent to Discord, checked when the user comes back to log in
const STATE_COOKIE: &str = "discord_oauth_state";

#[derive(serde::Deserialize)]
struct AuthorizeParams {
    /// Set to `consent` to always show Discord's authorization screen
    prompt: Option<String>,
}

/// Sends the user to Discord. With `prompt=none`, users who already authorized the app skip
/// Discord's authorization screen and come straight back.
async fn authorize(
    State(config): State<Arc<Config>>,
    Query(params): Query<AuthorizeParams>,
    jar: PrivateCookieJar,
) -> impl IntoResponse {
    let state = uuid::Uuid::new_v4().to_string();
    let prompt = match params.prompt.as_deref() {
        Some("consent") => "consent",
        _ => "none",
    };

    let mut url = url::Url::parse(&config.oauth_authorize_link)
        .expect("Invalid Discord OAuth authorize link");
    url.query_pairs_mut()
        .append_pair("prompt", prompt)
        .append_pair("state", &state);

    let jar = jar.add(
        Cookie::build((STATE_COOKIE, state))
            .path("/")
            .http_only(true)
            .secure(true)
            // Sent back on the redirect from Discord, a top-level navigation
            .same_site(SameSite::Lax)
            .max_age(time::Duration::minutes(10)),
    );
    (jar, response::Redirect::temporary(url.as_str()))
}

#[derive(serde::Deserialize)]
struct OAuthCallbackParams {
    code: String,
    state: String,
}

#[derive(Debug, thiserror::Error)]
enum OauthCallbackError {
    /// No login started from this browser in the last 10 minutes, or a forged callback
    #[error("Login expired or invalid, please try again")]
    InvalidState,

    #[error("Discord token error: {0}")]
    Token(#[from] TokenError),

    #[error("Storage error: {0}")]
    Storage(#[from] globibot_core::storage::StorageError),
}

impl IntoResponse for OauthCallbackError {
    fn into_response(self) -> response::Response {
        let status = match self {
            Self::InvalidState | Self::Token(TokenError::Rejected(_)) => StatusCode::BAD_REQUEST,
            Self::Token(TokenError::Request(_)) | Self::Storage(_) => {
                StatusCode::INTERNAL_SERVER_ERROR
            }
        };
        (status, format!("OAuth callback error: {self}")).into_response()
    }
}

async fn login(
    State(mut web_server): State<WebServer>,
    jar: PrivateCookieJar,
    Json(params): Json<OAuthCallbackParams>,
) -> Result<impl IntoResponse, OauthCallbackError> {
    // The login must have started from this browser (CSRF protection)
    let state_matches = jar
        .get(STATE_COOKIE)
        .is_some_and(|cookie| cookie.value() == params.state);
    if !state_matches {
        return Err(OauthCallbackError::InvalidState);
    }
    let jar = jar.remove(Cookie::build(STATE_COOKIE).path("/"));

    let config = &web_server.oauth_config;
    let authorize_url = url::Url::parse(&config.oauth_authorize_link)
        .expect("Invalid Discord OAuth authorize link");
    let redirect_uri = authorize_url
        .query_pairs()
        .find_map(|(k, v)| (k == "redirect_uri").then_some(v))
        .expect("redirect_uri not found in Discord OAuth authorize link");

    tracing::info!("Exchanging OAuth code for token...");
    let session = request_token(
        config,
        &[
            ("grant_type", "authorization_code"),
            ("code", &params.code),
            ("redirect_uri", &redirect_uri),
        ],
    )
    .await?;

    let session_id = uuid::Uuid::new_v4().to_string();
    save_session(&mut web_server.storage, &session_id, &session).await?;
    tracing::info!("Session stored");

    let jar = jar.add(session_cookie(session_id));
    Ok(profile(State(web_server), jar).await)
}

/// The `discord_session` cookie holds a session ID, the session itself (Discord tokens) is in
/// Redis. Both last 30 days from the last visit: every `/profile` call extends them.
const SESSION_COOKIE: &str = "discord_session";
const SESSION_LIFETIME: time::Duration = time::Duration::days(30);

/// How long a Discord profile is reused before asking Discord again. Plugins read it to
/// identify users, so it's also how long a membership change takes to apply.
const PROFILE_CACHE_SECS: i64 = 24 * 3_600;

fn session_cookie(session_id: String) -> Cookie<'static> {
    Cookie::build((SESSION_COOKIE, session_id))
        .path("/")
        .http_only(true)
        .secure(true)
        .same_site(SameSite::Lax)
        .max_age(SESSION_LIFETIME)
        .build()
}

fn session_cookie_removal() -> Cookie<'static> {
    Cookie::build(SESSION_COOKIE).path("/").build()
}

async fn save_session(
    storage: &mut globibot_core::storage::RedisStorage,
    session_id: &str,
    session: &DiscordSession,
) -> Result<(), globibot_core::storage::StorageError> {
    storage.set(session_id, session).await?;
    storage
        .expire::<DiscordSession>(session_id, SESSION_LIFETIME.whole_seconds())
        .await
}

#[derive(Debug, thiserror::Error)]
enum TokenError {
    /// The code or refresh token is invalid, expired or revoked
    #[error("rejected by Discord ({0})")]
    Rejected(reqwest::StatusCode),
    #[error(transparent)]
    Request(#[from] reqwest::Error),
}

/// Calls Discord's token endpoint, for a login code or a refresh token
async fn request_token(
    config: &Config,
    form: &[(&str, &str)],
) -> Result<DiscordSession, TokenError> {
    #[derive(serde::Deserialize)]
    struct DiscordToken {
        access_token: String,
        expires_in: i64,
        refresh_token: String,
    }

    let response = reqwest::Client::new()
        .post("https://discord.com/api/v10/oauth2/token")
        .basic_auth(config.app_id, Some(&config.app_secret))
        .form(form)
        .send()
        .await?;
    // Discord answers 400 `invalid_grant` for bad codes and refresh tokens
    if matches!(response.status().as_u16(), 400 | 401) {
        return Err(TokenError::Rejected(response.status()));
    }
    let token = response.error_for_status()?.json::<DiscordToken>().await?;

    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    Ok(DiscordSession {
        access_token: token.access_token,
        refresh_token: token.refresh_token,
        expires_at_ts: now + token.expires_in,
    })
}

async fn logout(State(mut web_server): State<WebServer>, jar: PrivateCookieJar) -> Response {
    if let Some(cookie) = jar.get(SESSION_COOKIE) {
        delete_session(&mut web_server, cookie.value()).await;
    }
    // Logged out in this browser even if the session couldn't be deleted
    (jar.remove(session_cookie_removal()), "Logged out").into_response()
}

/// Deletes the session and its cached profile: plugins identify users from the profile
async fn delete_session(web_server: &mut WebServer, session_id: &str) {
    let storage = &mut web_server.storage;
    if let Err(err) = storage.del::<DiscordSession>(session_id).await {
        tracing::warn!("Failed to delete Discord session: {err}");
    }
    if let Err(err) = storage.del::<DiscordProfile>(session_id).await {
        tracing::warn!("Failed to delete Discord profile: {err}");
    }
}

#[derive(Debug, thiserror::Error)]
enum ProfileError {
    /// The session can't be used anymore: the user has to log in again
    #[error("{1}")]
    LoggedOut(StatusCode, &'static str),
    /// Discord or Redis failed, the session may still be fine
    #[error("{0}")]
    Unavailable(String),
}

impl From<globibot_core::storage::StorageError> for ProfileError {
    fn from(err: globibot_core::storage::StorageError) -> Self {
        Self::Unavailable(format!("Storage error: {err}"))
    }
}

async fn profile(State(mut server): State<WebServer>, jar: PrivateCookieJar) -> Response {
    let Some(session_id) = jar
        .get(SESSION_COOKIE)
        .map(|cookie| cookie.value().to_string())
    else {
        return (StatusCode::UNAUTHORIZED, "No session").into_response();
    };

    match load_profile(&mut server, &session_id).await {
        // Sent again to extend it
        Ok(profile) => (jar.add(session_cookie(session_id)), Json(profile)).into_response(),
        Err(ProfileError::LoggedOut(status, reason)) => {
            delete_session(&mut server, &session_id).await;
            (status, jar.remove(session_cookie_removal()), reason).into_response()
        }
        Err(ProfileError::Unavailable(err)) => {
            tracing::warn!("Failed to load Discord profile: {err}");
            (
                StatusCode::BAD_GATEWAY,
                "Couldn't reach Discord, try again later",
            )
                .into_response()
        }
    }
}

async fn load_profile(
    server: &mut WebServer,
    session_id: &str,
) -> Result<DiscordProfile, ProfileError> {
    let storage = &mut server.storage;

    let Some(mut session) = storage.get_opt::<DiscordSession>(session_id).await? else {
        return Err(ProfileError::LoggedOut(
            StatusCode::UNAUTHORIZED,
            "Session expired",
        ));
    };
    storage
        .expire::<DiscordSession>(session_id, SESSION_LIFETIME.whole_seconds())
        .await?;

    if let Some(profile) = storage.get_opt::<DiscordProfile>(session_id).await? {
        return Ok(profile);
    }

    // Discord access tokens last a week, the session can outlive them
    let now = time::OffsetDateTime::now_utc().unix_timestamp();
    if session.expires_at_ts <= now + 60 {
        tracing::info!("Refreshing Discord access token...");
        let form = [
            ("grant_type", "refresh_token"),
            ("refresh_token", session.refresh_token.as_str()),
        ];
        session = match request_token(&server.oauth_config, &form).await {
            Ok(session) => session,
            Err(TokenError::Rejected(_)) => {
                return Err(ProfileError::LoggedOut(
                    StatusCode::UNAUTHORIZED,
                    "Discord authorization expired or was revoked",
                ));
            }
            Err(err) => return Err(ProfileError::Unavailable(err.to_string())),
        };
        save_session(storage, session_id, &session).await?;
    }

    tracing::info!("Fetching Discord profile...");
    let http = serenity::http::Http::new(&format!("Bearer {}", session.access_token));
    let member = match http
        .get_current_user_guild_member(server.oauth_config.guild_id)
        .await
    {
        Ok(member) => member,
        Err(serenity::Error::Http(err)) => match err.status_code().map(|status| status.as_u16()) {
            Some(404) => {
                return Err(ProfileError::LoggedOut(
                    StatusCode::FORBIDDEN,
                    "You are not a member of the required Discord guild",
                ));
            }
            Some(401) => {
                return Err(ProfileError::LoggedOut(
                    StatusCode::UNAUTHORIZED,
                    "Discord authorization was revoked",
                ));
            }
            _ => return Err(ProfileError::Unavailable(err.to_string())),
        },
        Err(err) => return Err(ProfileError::Unavailable(err.to_string())),
    };

    let profile = DiscordProfile {
        user_id: member.user.id,
        username: member.display_name().to_string(),
        avatar_url: member.avatar_url().or(member.user.avatar_url()),
    };

    let storage = &mut server.storage;
    if let Err(err) = storage.set(session_id, &profile).await {
        tracing::warn!("Failed to cache Discord profile: {err}");
    }
    if let Err(err) = storage
        .expire::<DiscordProfile>(session_id, PROFILE_CACHE_SECS)
        .await
    {
        tracing::warn!("Failed to set expiration for Discord profile: {err}");
    }

    Ok(profile)
}
