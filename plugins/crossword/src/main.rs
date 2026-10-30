use std::error::Error;

use globibot_core::{
    events::{Event, EventType},
    plugin::{HandleEvents, HasEvents, HasRpc, Plugin},
    rpc::{self, context::current as rpc_context},
    serenity::all::CommandId,
};

type PluginError = Box<dyn Error + Send + Sync>;

/// Interaction response type that opens the application's Activity for the
/// user. Serenity 0.12 has no builder for it, but the RPC forwards raw JSON.
const LAUNCH_ACTIVITY: u8 = 12;

#[tokio::main]
async fn main() -> common::anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let endpoints = common::endpoints::tcp_from_env([EventType::InteractionCreate])?;

    let desired_command: serde_json::Value =
        serde_json::from_str(include_str!("../crossword-slash-command.json"))?;

    let plugin = CrosswordPlugin::connect_init(endpoints, async |rpc| {
        let command = rpc
            .upsert_global_command(rpc_context(), desired_command)
            .await??;

        common::anyhow::Ok(CrosswordPlugin {
            command_id: command.id,
        })
    })
    .await?;

    plugin.handle_events().await?;

    Ok(())
}

/// Launches the Mini Crossword Activity from `/crossword`. The game itself is
/// a separate web app (the `mini-crossword` repo) served as this application's
/// Activity, so Activities must be enabled for the app in the Developer Portal.
struct CrosswordPlugin {
    command_id: CommandId,
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
            Event::InteractionCreate { interaction } if interaction.data.id == self.command_id => {
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
