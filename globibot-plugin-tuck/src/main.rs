#![feature(type_alias_impl_trait)]

use std::{
    error::Error,
    path::PathBuf,
    sync::{
        atomic::{AtomicUsize, Ordering::SeqCst},
        Arc,
    },
    time::Instant,
};

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
    load_avatar, load_gif, paste_avatar, AvatarPositions, Dimension, PasteAvatarPositions,
};
use image::RgbaImage;

type PluginError = Box<dyn Error + Send + Sync>;

#[derive(Debug, Clone)]
struct TuckGifDescriptor {
    file_name: &'static str,
    dimension: Dimension,
    avatar_dimensions: Dimension,
    avatar_positions: AvatarPositions,
}

const TUCK_GIF_DESCRIPTORS: [TuckGifDescriptor; 2] = [
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
    },
    TuckGifDescriptor {
        file_name: "tuck-kitties.gif",
        dimension: (129, 249),
        avatar_dimensions: (42, 42),
        avatar_positions: |frame_idx| PasteAvatarPositions {
            tucked_position: Some((10, 122)),
            tucker_position: Some((65 - (frame_idx % 14) / 2, 90)),
        },
    },
];

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let tuck_gifs = TUCK_GIF_DESCRIPTORS.map(|d| {
        let mut img_path = PathBuf::from(load_env("TUCK_IMG_PATH"));
        img_path.push(d.file_name);
        let gif = load_gif(img_path, d.dimension).expect("Failed to load gif");
        (d, gif)
    });

    let plugin = TuckPlugin {
        command_id: load_env("TUCK_COMMAND_ID")
            .parse()
            .expect("Malformed command id"),
        tuck_gifs,
        tuck_gif_idx: AtomicUsize::new(0),
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
    tuck_gif_idx: AtomicUsize,
}

impl<const GIF_COUNT: usize> TuckPlugin<GIF_COUNT> {
    async fn generate_tucking_gif(
        &self,
        tucker_avatar_url: &str,
        tucked_avatar_url: &str,
    ) -> Result<Vec<u8>, PluginError> {
        let idx = self.tuck_gif_idx.fetch_add(1, SeqCst);
        let (tuck_desc, tuck_gif) = self.tuck_gifs[idx % self.tuck_gifs.len()].clone();

        let avatars = futures::try_join!(
            load_avatar(tucker_avatar_url, tuck_desc.avatar_dimensions),
            load_avatar(tucked_avatar_url, tuck_desc.avatar_dimensions),
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
                        .first()
                        .and_then(|opt| opt.resolved.as_ref())
                    {
                        Some(ApplicationCommandInteractionDataOptionValue::User(u, _)) => u.clone(),
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
                                .generate_tucking_gif(&tucker_avatar_url, &tucked_avatar_url)
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
