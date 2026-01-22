mod discord_oauth;
pub mod plugins;

use std::sync::Arc;

use axum_extra::extract::cookie::Key;
use globibot_core::{serenity::all::GuildId, storage::RedisStorage};
use tokio::net;

#[derive(Debug, Clone, axum::extract::FromRef)]
pub struct WebServer {
    storage: RedisStorage,
    cookie_key: Key,
    oauth_config: Arc<discord_oauth::Config>,
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
            oauth_config: Arc::new(discord_oauth::Config {
                app_id: discord_app_id,
                app_secret: discord_app_secret,
                guild_id: discord_guild_id,
                oauth_authorize_link: discord_oauth_authorize_link,
            }),
        }
    }

    pub async fn serve(self, addr: impl net::ToSocketAddrs) -> std::io::Result<()> {
        use axum::routing::get;

        let app = axum::Router::new()
            .route("/", get(async || "Globibot Web Server"))
            .nest("/plugins", plugins::router())
            .nest("/discord", discord_oauth::router().with_state(self));

        let listener = net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }
}
