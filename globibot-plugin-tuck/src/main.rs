use std::{convert::TryInto, error::Error, path::PathBuf, time::Instant};

use common::image::RgbaImage;
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
use globibot_plugin_tuck::{
    AvatarPositions, Dimension, PasteAvatarPositions, load_gif, paste_avatar,
};
use rand::Rng;

type PluginError = Box<dyn Error + Send + Sync>;

#[derive(Debug, Clone)]
struct TuckGifDescriptor {
    file_name: &'static str,
    dimension: Dimension,
    avatar_dimensions: Dimension,
    avatar_positions: AvatarPositions,
    frame_range: Option<std::ops::Range<usize>>,
}

const TUCK_GIF_DESCRIPTORS: [TuckGifDescriptor; 4] = [
    TuckGifDescriptor {
        file_name: "eyebleach-tuck.gif",
        dimension: (160, 154),
        avatar_dimensions: (50, 50),
        avatar_positions: |frame_idx| {
            let x = 45_i64 + (frame_idx / 5 * 2);
            let y = 26_i64.saturating_sub(frame_idx * 5 / 4).max(5);

            PasteAvatarPositions {
                tucked_position: Some((x, y)),
                tucker_position: None,
            }
        },
        frame_range: None,
    },
    TuckGifDescriptor {
        file_name: "tuck-kitties.gif",
        dimension: (129, 249),
        avatar_dimensions: (42, 42),
        avatar_positions: |frame_idx| PasteAvatarPositions {
            tucked_position: Some((10, 122)),
            tucker_position: Some((65 - (frame_idx % 14) / 2, 90)),
        },
        frame_range: None,
    },
    TuckGifDescriptor {
        file_name: "tuck-kitty-bman.gif",
        dimension: (150, 150),
        avatar_dimensions: (60, 60),
        avatar_positions: |frame_idx| PasteAvatarPositions {
            tucked_position: Some((35 + frame_idx / 10, 32 + frame_idx / 30)),
            tucker_position: None,
        },
        frame_range: Some(50..150),
    },
    TuckGifDescriptor {
        file_name: "cat-bed.gif",
        dimension: (320, 180),
        avatar_dimensions: (60, 60),
        avatar_positions: |frame_idx| {
            let y = match frame_idx {
                0 => 40,
                1 => 50,
                2 => 60,
                3 => 70,
                4 => 80,
                _ => 90,
            };

            PasteAvatarPositions {
                tucked_position: Some((180, y)),
                tucker_position: None,
            }
        },
        frame_range: None,
    },
];

#[tokio::main]
async fn main() -> common::anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let tuck_gifs = TUCK_GIF_DESCRIPTORS.map(|d| {
        let mut img_path = PathBuf::from(load_env("TUCK_IMG_PATH"));
        img_path.push(d.file_name);
        let mut gif = load_gif(img_path, d.dimension).expect("Failed to load gif");
        if let Some(range) = &d.frame_range {
            gif = gif.drain(range.clone()).collect();
        }
        (d, gif)
    });

    let events = [EventType::MessageCreate, EventType::InteractionCreate];

    let endpoints = Endpoints::new()
        .rpc(Tcp::new(load_env("RPC_ADDR")))
        .events(Tcp::new(load_env("SUBSCRIBER_ADDR")), events);

    let desired_command: serde_json::Value =
        serde_json::from_str(include_str!("../tuck-slash-command.json"))?;

    let plugin = TuckPlugin::connect_init(endpoints, async |rpc| {
        let command = rpc
            .upsert_global_command(rpc_context(), desired_command)
            .await
            .expect("Failed to perform rpc query")
            .expect("Failed to upsert guild command");

        TuckPlugin {
            command_id: command.id,
            tuck_gifs,
        }
    })
    .await?;

    plugin.handle_events().await?;

    Ok(())
}

fn load_env(key: &str) -> String {
    std::env::var(key)
        .unwrap_or_else(|why| panic!("Failed to load environment variable '{}': {}", key, why))
}

struct TuckPlugin<const GIF_COUNT: usize> {
    command_id: CommandId,
    tuck_gifs: [(TuckGifDescriptor, Vec<RgbaImage>); GIF_COUNT],
}

impl<const GIF_COUNT: usize> TuckPlugin<GIF_COUNT> {
    async fn generate_tucking_gif(
        &self,
        tucker_avatar_url: &str,
        tucked_avatar_url: &str,
        gif_idx: Option<usize>,
    ) -> Result<Vec<u8>, PluginError> {
        let idx = gif_idx.unwrap_or_else(|| rand::rng().random_range(0..self.tuck_gifs.len()));
        let (tuck_desc, tuck_gif) = self.tuck_gifs[idx].clone();

        let avatars = futures::try_join!(
            common::imageops::load_avatar(tucker_avatar_url, tuck_desc.avatar_dimensions),
            common::imageops::load_avatar(tucked_avatar_url, tuck_desc.avatar_dimensions),
        )?;

        let t0 = Instant::now();
        let gif = tokio::task::spawn_blocking(move || {
            paste_avatar(
                (tuck_gif, tuck_desc.dimension),
                avatars,
                tuck_desc.avatar_positions,
            )
        })
        .await??;
        tracing::info!("Generated image in {}ms", t0.elapsed().as_millis());

        Ok(gif)
    }
}

impl<const GIF_COUNT: usize> Plugin for TuckPlugin<GIF_COUNT> {
    const ID: &'static str = "Tuck";

    type RpcPolicy = HasRpc<true>;
    type EventsPolicy = HasEvents<true>;
}

impl<const GIF_COUNT: usize> HandleEvents for TuckPlugin<GIF_COUNT> {
    type Err = PluginError;

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
                let user_id_to_tuck = match command
                    .options
                    .iter()
                    .find(|opt| opt.name == "target")
                    .map(|opt| &opt.value)
                {
                    Some(&CommandDataOptionValue::User(user_id)) => user_id,
                    _ => return Ok(()),
                };
                let gif_idx = match command
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

                let tucker_avatar_url = author
                    .avatar_url()
                    .unwrap_or_else(|| author.default_avatar_url());

                let user_to_tuck = rpc.get_user(rpc_context(), user_id_to_tuck).await??;
                let tucked_avatar_url = user_to_tuck
                    .avatar_url()
                    .unwrap_or_else(|| user_to_tuck.default_avatar_url());

                rpc.create_interaction_response(
                    rpc_context(),
                    id,
                    token.clone(),
                    serde_json::json!({
                        "type": 4,
                        "data": {
                            "content": format!(
                                "{} is fetching some blankets for {}",
                                author.mention(), user_id_to_tuck.mention()
                            )
                        }
                    }),
                )
                .await??;

                let gif = self
                    .generate_tucking_gif(&tucker_avatar_url, &tucked_avatar_url, gif_idx)
                    .await?;
                tracing::info!("Sending gif of {} bytes", gif.len());

                rpc.send_file(rpc_context(), channel_id, gif, "tuck.gif".to_owned())
                    .await??;
            }
            _ => (),
        }
        Ok(())
    }
}
