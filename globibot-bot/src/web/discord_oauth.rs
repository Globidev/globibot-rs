use std::sync::Arc;

use axum::{
    Json,
    extract::State,
    http::StatusCode,
    response::{self, IntoResponse, Response},
};
use axum_extra::extract::{PrivateCookieJar, cookie::Cookie};
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

async fn authorize(State(config): State<Arc<Config>>) -> impl IntoResponse {
    response::Redirect::temporary(&config.oauth_authorize_link)
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

async fn login(
    State(mut web_server): State<WebServer>,
    jar: PrivateCookieJar,
    Json(params): Json<OAuthCallbackParams>,
) -> Result<impl IntoResponse, OauthCallbackError> {
    let config = &web_server.oauth_config;
    let now = time::OffsetDateTime::now_utc();

    let client = reqwest::Client::new();

    let authorize_url = url::Url::parse(&config.oauth_authorize_link)
        .expect("Invalid Discord OAuth authorize link");
    let redirect_uri = authorize_url
        .query_pairs()
        .find_map(|(k, v)| (k == "redirect_uri").then_some(v))
        .expect("redirect_uri not found in Discord OAuth authorize link");

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
        .basic_auth(config.app_id, Some(&config.app_secret))
        .form(&[
            ("grant_type", "authorization_code"),
            ("code", &params.code),
            ("redirect_uri", &redirect_uri),
        ])
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
    web_server.storage.set(session_id, &discord_session).await?;

    tracing::info!("Session stored, setting cookie...");

    let jar = jar.add(
        Cookie::build(("discord_session", session_id.to_string()))
            .path("/")
            .http_only(true)
            .secure(true)
            .expires(expiry_date), // 🔶 Might not be needed when supporting automatic token refreshes
    );

    Ok(profile(State(web_server), jar).await)
}

async fn logout(
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

async fn profile(State(mut server): State<WebServer>, jar: PrivateCookieJar) -> Response {
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
        .get_current_user_guild_member(server.oauth_config.guild_id)
        .await
    {
        Ok(user) => user,
        Err(serenity::Error::Http(err))
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
        user_id: member.user.id,
        username: member.display_name().to_string(),
        avatar_url: member.avatar_url().or(member.user.avatar_url()),
    };

    if let Err(err) = server.storage.set(session_id, &profile).await {
        tracing::warn!("Failed to cache Discord profile: {err}");
    }
    if let Err(err) = server
        .storage
        .expire::<DiscordProfile>(session_id, 24 * 3_600)
        .await
    {
        tracing::warn!("Failed to set expiration for Discord profile: {err}");
    }

    (jar, Json(profile)).into_response()
}
