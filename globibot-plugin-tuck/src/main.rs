#![feature(type_alias_impl_trait)]

use std::{convert::TryInto, error::Error, path::PathBuf, sync::Arc, time::Instant};

use globibot_core::{
    events::{Event, EventType},
    plugin::{Endpoints, HandleEvents, HasEvents, HasRpc, Plugin, PluginExt},
    rpc::{self, context::current as rpc_context},
    serenity::{
        model::{
            guild::Member,
            interactions::application_command::{
                ApplicationCommandInteraction, ApplicationCommandInteractionDataOptionValue,
            },
        },
        prelude::Mentionable,
    },
    transport::Tcp,
};
use globibot_plugin_tuck::{
    load_gif, paste_avatar, AvatarPositions, Dimension, PasteAvatarPositions,
};
use image::RgbaImage;
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
            let x = 45 + (frame_idx / 5 * 2);
            let y = 26_u32.saturating_sub(frame_idx * 5 / 4).max(5);

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
async fn main() {
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

    let plugin = TuckPlugin {
        command_id: load_env("TUCK_COMMAND_ID")
            .parse()
            .expect("Malformed command id"),
        tuck_gifs,
    };

    let events = [EventType::MessageCreate, EventType::InteractionCreate];

    let endpoints = Endpoints::new()
        .rpc(Tcp::new(load_env("RPC_ADDR")))
        .events(Tcp::new(load_env("SUBSCRIBER_ADDR")), events);

    plugin
        .connect(endpoints)
        .await
        .expect("Failed to connect plugin")
        .handle_events()
        .await
        .expect("Failed to run plugin");
}

fn load_env(key: &str) -> String {
    std::env::var(key)
        .unwrap_or_else(|why| panic!("Failed to load environment variable '{}': {}", key, why))
}

struct TuckPlugin<const GIF_COUNT: usize> {
    command_id: u64,
    tuck_gifs: [(TuckGifDescriptor, Vec<RgbaImage>); GIF_COUNT],
}

impl<const GIF_COUNT: usize> TuckPlugin<GIF_COUNT> {
    async fn generate_tucking_gif(
        &self,
        tucker_avatar_url: &str,
        tucked_avatar_url: &str,
        gif_idx: Option<usize>,
    ) -> Result<Vec<u8>, PluginError> {
        let idx = gif_idx.unwrap_or_else(|| rand::thread_rng().gen_range(0..self.tuck_gifs.len()));
        let (tuck_desc, tuck_gif) = self.tuck_gifs[idx].clone();

        let avatars = futures::try_join!(
            globibot_plugin_common::imageops::load_avatar(
                tucker_avatar_url,
                tuck_desc.avatar_dimensions
            ),
            globibot_plugin_common::imageops::load_avatar(
                tucked_avatar_url,
                tuck_desc.avatar_dimensions
            ),
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
    type Future = impl std::future::Future<Output = Result<(), Self::Err>>;

    fn on_event(self: Arc<Self>, rpc: rpc::ProtocolClient, event: Event) -> Self::Future {
        async move {
            match event {
                Event::MessageCreate { message: _ } => {}
                Event::InteractionCreate {
                    interaction:
                        ApplicationCommandInteraction {
                            id,
                            token,
                            data: command,
                            channel_id,
                            member: Some(Member { user: author, .. }),
                            ..
                        },
                } if command.id == self.command_id => {
                    let user_to_tuck = match command
                        .options
                        .iter()
                        .find(|opt| opt.name == "target")
                        .and_then(|opt| opt.resolved.as_ref())
                    {
                        Some(ApplicationCommandInteractionDataOptionValue::User(u, _)) => u.clone(),
                        _ => return Ok(()),
                    };
                    let gif_idx = match command
                        .options
                        .iter()
                        .find(|opt| opt.name == "flavor")
                        .and_then(|opt| opt.resolved.as_ref())
                    {
                        Some(ApplicationCommandInteractionDataOptionValue::Integer(flavor_idx)) => {
                            Some((*flavor_idx).try_into().unwrap_or(0))
                        }
                        None => None,
                        _ => return Ok(()),
                    };

                    let tucker_avatar_url = author
                        .avatar_url()
                        .unwrap_or_else(|| author.default_avatar_url());

                    let tucked_avatar_url = user_to_tuck
                        .avatar_url()
                        .unwrap_or_else(|| user_to_tuck.default_avatar_url());

                    let generate_gif = tokio::spawn({
                        let plugin = Arc::clone(&self);
                        async move {
                            plugin
                                .generate_tucking_gif(
                                    &tucker_avatar_url,
                                    &tucked_avatar_url,
                                    gif_idx,
                                )
                                .await
                        }
                    });

                    rpc.create_interaction_response(
                        rpc_context(),
                        id.0,
                        token.clone(),
                        serde_json::json!({
                            "type": 4,
                            "data": {
                                "content": format!(
                                    "{} is fetching some blankets for {}",
                                    author.mention(), user_to_tuck.mention()
                                )
                            }
                        }),
                    )
                    .await??;

                    let gif = generate_gif.await??;
                    tracing::info!("Sending gif of {} bytes", gif.len());

                    rpc.send_file(rpc_context(), channel_id, gif, "tuck.gif".to_owned())
                        .await??;
                }
                _ => (),
            }
            Ok(())
        }
    }
}
