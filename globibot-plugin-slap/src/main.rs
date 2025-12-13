use common::{
    anyhow,
    imageops::{self, Avatar, GifBuilder},
};
use globibot_core::{
    events::{Event, EventType},
    plugin::{Endpoints, HandleEvents, HasEvents, HasRpc, Plugin},
    rpc::{self, context::current as rpc_context},
    serenity::{
        all::CommandId,
        model::application::{CommandDataOptionValue, CommandInteraction},
        prelude::Mentionable,
    },
    transport::Tcp,
};
use rand::Rng;
use std::time::Instant;

pub mod scenario {
    pub mod animated_slap;
    pub mod static_slap;

    use std::path::PathBuf;

    fn img_path(file_name: &str) -> PathBuf {
        let mut path = PathBuf::from(common::load_env("SLAP_IMG_PATH"));
        path.push(file_name);
        path
    }

    #[derive(Debug, Clone)]
    pub struct SlapScenario {
        pub dim: (u16, u16),
        pub avatar_dim: (u16, u16),
        pub slapper_positions: Vec<(i64, i64)>,
        pub slapped_positions: Vec<(i64, i64)>,
        pub frames: Vec<common::image::RgbaImage>,
    }
}

use scenario::SlapScenario;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let guild_id = std::env::var("SLAP_INSTALL_COMMAND_GUILD_ID")?.parse()?;
    let desired_command: serde_json::Value =
        serde_json::from_str(include_str!("../slap-slash-command.json"))?;

    let events = [EventType::MessageCreate, EventType::InteractionCreate];
    let endpoints = Endpoints::new()
        .rpc(Tcp::new(common::load_env("RPC_ADDR")))
        .events(Tcp::new(common::load_env("SUBSCRIBER_ADDR")), events);

    let slap_scenarios = vec![
        scenario::static_slap::load_scenario()?,
        scenario::animated_slap::load_scenario()?,
    ];

    let plugin = SlapPlugin::connect_init(endpoints, async |rpc| {
        let command = rpc
            .upsert_guild_command(rpc_context(), guild_id, desired_command)
            .await??;

        anyhow::Ok(SlapPlugin {
            command_id: command.id,
            slap_scenarios,
        })
    })
    .await?;

    plugin.handle_events().await?;

    Ok(())
}

struct SlapPlugin {
    command_id: CommandId,
    slap_scenarios: Vec<SlapScenario>,
}

impl SlapPlugin {
    async fn generate_slapping_gif(
        &self,
        slapper_avatar_url: &str,
        slapped_avatar_url: &str,
        descriptor_idx: Option<usize>,
    ) -> Result<Vec<u8>, anyhow::Error> {
        let idx = descriptor_idx
            .unwrap_or_else(|| rand::rng().random_range(0..self.slap_scenarios.len()));
        let scenario = self.slap_scenarios[idx].clone();

        let (slapper_avatar, slapped_avatar) = futures::try_join!(
            imageops::load_avatar(slapper_avatar_url, scenario.avatar_dim),
            imageops::load_avatar(slapped_avatar_url, scenario.avatar_dim),
        )?;

        let t0 = Instant::now();
        let gif = tokio::task::spawn_blocking(move || {
            let mut builder = GifBuilder::from_background_frames(scenario.frames, scenario.dim);

            match slapper_avatar {
                Avatar::Animated(frames) => builder.overlay(&frames, &scenario.slapper_positions),
                Avatar::Fixed(frame) => builder.overlay(&[frame], &scenario.slapper_positions),
            };

            match slapped_avatar {
                Avatar::Animated(frames) => builder.overlay(&frames, &scenario.slapped_positions),
                Avatar::Fixed(frame) => builder.overlay(&[frame], &scenario.slapped_positions),
            };

            builder.finish()
        })
        .await??;
        tracing::info!("Generated image in {}ms", t0.elapsed().as_millis());

        Ok(gif)
    }
}

impl Plugin for SlapPlugin {
    const ID: &'static str = "Slap";

    type RpcPolicy = HasRpc<true>;
    type EventsPolicy = HasEvents<true>;
}

impl HandleEvents for SlapPlugin {
    type Err = anyhow::Error;

    async fn on_event(&self, rpc: rpc::ProtocolClient, event: Event) -> Result<(), Self::Err> {
        match event {
            Event::MessageCreate { message: _ } => {}
            Event::InteractionCreate { interaction } if interaction.data.id == self.command_id => {
                let CommandInteraction {
                    id,
                    token,
                    data: command,
                    channel_id,
                    user: author,
                    ..
                } = *interaction;
                let user_id_to_slap = match command
                    .options
                    .iter()
                    .find(|opt| opt.name == "target")
                    .map(|opt| &opt.value)
                {
                    Some(&CommandDataOptionValue::User(user_id)) => user_id,
                    _ => return Ok(()),
                };

                let descriptor_idx = match command
                    .options
                    .iter()
                    .find(|opt| opt.name == "flavor")
                    .map(|opt| &opt.value)
                {
                    Some(&CommandDataOptionValue::Integer(flavor_idx)) => {
                        Some(flavor_idx.try_into().unwrap_or(0))
                    }
                    None => None,
                    _ => return Ok(()),
                };

                let slapper_avatar_url = author
                    .avatar_url()
                    .unwrap_or_else(|| author.default_avatar_url());

                let user_to_slap = rpc.get_user(rpc_context(), user_id_to_slap).await??;
                let slapped_avatar_url = user_to_slap
                    .avatar_url()
                    .unwrap_or_else(|| user_to_slap.default_avatar_url());

                rpc.create_interaction_response(
                    rpc_context(),
                    id,
                    token,
                    serde_json::json!({
                        "type": 4,
                        "data": {
                            "content": format!(
                                "{} walks angrily towards {}",
                                author.mention(), user_to_slap.mention()
                            )
                        }
                    }),
                )
                .await??;

                let gif = self
                    .generate_slapping_gif(&slapper_avatar_url, &slapped_avatar_url, descriptor_idx)
                    .await?;
                tracing::info!("Sending gif of {} bytes", gif.len());

                rpc.send_file(rpc_context(), channel_id, gif, "slap.gif".to_owned())
                    .await??;
            }
            _ => (),
        }
        Ok(())
    }
}
