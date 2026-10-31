mod leaderboard;
mod web;

use std::{error::Error, sync::Arc};

use common::anyhow::Context;
use globibot_core::{
    bot_command, bot_commands,
    events::{Event, EventType},
    plugin::{HandleEvents, HasEvents, HasRpc, Plugin},
    rpc::{self, context::current as rpc_context},
    storage::RedisStorage,
};
use leaderboard::{CrosswordResults, RESULTS_KEY};
use tokio::sync::Mutex;
use web::WebServer;

type PluginError = Box<dyn Error + Send + Sync>;

/// Interaction response type that opens the application's Activity for the
/// user. Serenity 0.12 has no builder for it, but the RPC forwards raw JSON.
const LAUNCH_ACTIVITY: u8 = 12;

const DESCRIPTION: &str = "Play today's mini crossword and compete on the server leaderboard";

bot_commands! {
    enum CrosswordCommands {
        Crossword(Crossword),
        Launch(Launch),
    }
}

bot_command! {
    #[slash(scope = global, name = "crossword", description = DESCRIPTION)]
    struct Crossword;
}

bot_command! {
    /// App Launcher entry. Handled by us (`handler = 1`), not Discord, so launching doesn't post
    /// an invite card. Entry points only show in the App Launcher, hence `/crossword` too.
    #[entry_point(scope = global, name = "launch", description = DESCRIPTION, handler = 1)]
    struct Launch;
}

#[tokio::main]
async fn main() -> common::anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let web_addr_listen = std::env::var("CROSSWORD_WEB_ADDR_LISTEN")?;
    let web_addr_advertise = std::env::var("CROSSWORD_WEB_ADDR_ADVERTISE")?.parse()?;
    let cookie_secret = std::env::var("COOKIE_SECRET")?;
    let results_token = std::env::var("CROSSWORD_RESULTS_TOKEN")?;

    let endpoints = common::endpoints::tcp_from_env([EventType::InteractionCreate])?;

    let mut storage = RedisStorage::from_env().await?;

    let plugin = CrosswordPlugin::connect_init(endpoints, async |rpc| {
        CrosswordCommands::register(rpc, None).await?;

        rpc.register_web_api(rpc_context(), web_addr_advertise)
            .await??;

        let results = storage
            .get_opt::<CrosswordResults>(RESULTS_KEY)
            .await
            .context("Failed to load crossword results")?
            .unwrap_or_default();

        common::anyhow::Ok(CrosswordPlugin {
            results: Mutex::new(results),
        })
    })
    .await?;

    let web_server = WebServer::new(
        storage,
        &cookie_secret,
        &results_token,
        Arc::clone(&plugin.inner),
        plugin.rpc.clone(),
    );

    tokio::select! {
        _ = web_server.serve(web_addr_listen) => {
            tracing::info!("Web server has shut down");
        },
        _ = plugin.handle_events() => {
            tracing::info!("Event handler has shut down");
        },
    }

    Ok(())
}

/// Launches the Mini Crossword Activity from `/crossword` and keeps its
/// leaderboard. The game itself is a separate web app (the `mini-crossword`
/// repo) served as this application's Activity, so Activities must be enabled
/// for the app in the Developer Portal. That app reports solves to `/results`.
pub struct CrosswordPlugin {
    results: Mutex<CrosswordResults>,
}

impl Plugin for CrosswordPlugin {
    const ID: &'static str = "crossword";

    type RpcPolicy = HasRpc<true>;
    type EventsPolicy = HasEvents<true>;
}

impl HandleEvents for CrosswordPlugin {
    type Err = PluginError;

    async fn on_event(&self, rpc: rpc::ProtocolClient, event: Event) -> Result<(), Self::Err> {
        match event {
            // Both commands just launch the Activity
            Event::InteractionCreate { interaction }
                if CrosswordCommands::parse(&interaction).is_some() =>
            {
                rpc.create_interaction_response(
                    rpc_context(),
                    interaction.id,
                    interaction.token.clone(),
                    serde_json::json!({ "type": LAUNCH_ACTIVITY }),
                )
                .await??;
                tracing::info!(user = %interaction.user.id, "Launched crossword Activity");
            }
            _ => (),
        }
        Ok(())
    }
}
