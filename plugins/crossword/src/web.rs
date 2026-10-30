use std::sync::Arc;

use axum::{
    Json,
    extract::{FromRef, Request, State},
    http::{StatusCode, header::AUTHORIZATION},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use axum_extra::extract::{PrivateCookieJar, cookie::Key};
use globibot_core::{
    rpc::{self, context::current as rpc_context},
    serenity::all::UserId,
    storage::{DiscordProfile, RedisStorage},
};
use tokio::net::ToSocketAddrs;

use crate::{
    CrosswordPlugin,
    leaderboard::{Player, RESULTS_KEY, day_number},
};

#[derive(Clone, FromRef)]
pub struct WebServer {
    storage: RedisStorage,
    cookie_key: Key,
    plugin: Arc<CrosswordPlugin>,
    rpc: rpc::ProtocolClient,
    results_token: ResultsToken,
}

/// Shared secret the crossword app sends with its results, as `Authorization: Bearer <token>`
#[derive(Clone)]
pub struct ResultsToken(Arc<str>);

impl WebServer {
    pub fn new(
        storage: RedisStorage,
        cookie_secret: &str,
        results_token: &str,
        plugin: Arc<CrosswordPlugin>,
        rpc: rpc::ProtocolClient,
    ) -> Self {
        Self {
            storage,
            cookie_key: Key::from(cookie_secret.as_bytes()),
            plugin,
            rpc,
            results_token: ResultsToken(results_token.into()),
        }
    }

    pub async fn serve(self, addr: impl ToSocketAddrs) -> std::io::Result<()> {
        // The crossword app's server reports results with the token, people browse with Discord
        let results_api = axum::Router::new()
            .route("/results", axum::routing::post(submit_result))
            .route_layer(middleware::from_fn_with_state(self.clone(), token_auth));
        let leaderboard_api = axum::Router::new()
            .route("/leaderboard", axum::routing::get(leaderboard))
            .route_layer(middleware::from_fn_with_state(self.clone(), discord_auth));

        let app = results_api.merge(leaderboard_api).with_state(self);

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }
}

async fn leaderboard(State(plugin): State<Arc<CrosswordPlugin>>) -> impl IntoResponse {
    Json(plugin.results.lock().await.leaderboard())
}

#[derive(Debug, serde::Deserialize)]
struct SubmitResultRequest {
    user_id: UserId,
    /// The puzzle's date, `YYYY-MM-DD`
    date: String,
    solve_time_secs: u32,
}

async fn submit_result(
    State(plugin): State<Arc<CrosswordPlugin>>,
    State(rpc): State<rpc::ProtocolClient>,
    State(mut storage): State<RedisStorage>,
    Json(req): Json<SubmitResultRequest>,
) -> Response {
    if day_number(&req.date).is_none() {
        return (StatusCode::BAD_REQUEST, "`date` must be a YYYY-MM-DD date").into_response();
    }
    if req.solve_time_secs == 0 {
        return (
            StatusCode::BAD_REQUEST,
            "`solve_time_secs` must be positive",
        )
            .into_response();
    }

    // Refreshed on every solve so name and avatar changes show up on the leaderboard
    let user = match rpc.get_user(rpc_context(), req.user_id).await {
        Ok(Ok(user)) => user,
        Ok(Err(err)) => {
            tracing::warn!("Failed to fetch crossword player {}: {err:?}", req.user_id);
            return (StatusCode::BAD_REQUEST, "Unknown Discord user").into_response();
        }
        Err(err) => {
            tracing::error!("RPC error while fetching a crossword player: {err:?}");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };
    let player = Player {
        user_id: user.id,
        username: user.display_name().to_string(),
        avatar_url: user.avatar_url(),
    };

    // Saving while holding the lock keeps concurrent solves from being saved out of order
    let mut results = plugin.results.lock().await;
    let recorded = results.record(player, req.date, req.solve_time_secs);
    if let Err(err) = storage.set(RESULTS_KEY, &*results).await {
        tracing::error!("Failed to save crossword results: {err:?}");
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    }

    if recorded {
        StatusCode::CREATED.into_response()
    } else {
        (StatusCode::OK, "Already solved, the first solve is kept").into_response()
    }
}

async fn token_auth(
    State(ResultsToken(token)): State<ResultsToken>,
    req: Request,
    next: Next,
) -> Response {
    let provided = req
        .headers()
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "));

    if provided != Some(&*token) {
        return (StatusCode::UNAUTHORIZED, "Invalid results token").into_response();
    }

    next.run(req).await
}

async fn discord_auth(
    State(mut storage): State<RedisStorage>,
    jar: PrivateCookieJar,
    mut req: Request,
    next: Next,
) -> Response {
    let Some(cookie) = jar.get("discord_session") else {
        return (
            StatusCode::UNAUTHORIZED,
            "You need to be logged in to access this resource",
        )
            .into_response();
    };
    let session_id = cookie.value();
    let profile = match storage.get::<DiscordProfile>(session_id).await {
        Ok(profile) => profile,
        Err(err) => {
            tracing::error!("Failed to get Discord session from storage: {err:?}");
            return (StatusCode::UNAUTHORIZED, "Invalid session").into_response();
        }
    };

    req.extensions_mut().insert(profile);

    next.run(req).await
}
