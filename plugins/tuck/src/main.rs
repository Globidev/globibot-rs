use std::{error::Error, path::PathBuf, time::Instant};

use common::image::RgbaImage;
use globibot_core::{
    bot_choices, bot_command,
    command::{self, Command},
    events::{Event, EventType},
    plugin::{HandleEvents, HasEvents, HasRpc, Plugin},
    rpc::{self, context::current as rpc_context},
    serenity::{
        all::{CommandInteraction, UserId},
        prelude::Mentionable,
    },
};
use globibot_plugin_tuck::{
    AvatarPositions, Dimension, PasteAvatarPositions, load_gif, paste_avatar,
};
use rand::Rng;

type PluginError = Box<dyn Error + Send + Sync>;

bot_command! {
    #[slash(scope = global, name = "tuck",
            description = "Tuck someone you like to help them have a good night's sleep")]
    struct Tuck {
        #[option(name = "target", description = "The user to tuck")]
        target: UserId,
        #[option(name = "flavor", description = "The way you want to tuck them")]
        flavor: Option<TuckFlavor>,
    }
}

bot_choices! {
    /// Values are indices into the tuck GIFs
    enum TuckFlavor: i64 {
        #[choice(name = "eyebleach kitty 1", value = 0)]
        EyebleachKitty1,
        #[choice(name = "peach kitties", value = 1)]
        PeachKitties,
        #[choice(name = "eyebleach kitty 2", value = 2)]
        EyebleachKitty2,
        #[choice(name = "anime kitty", value = 3)]
        AnimeKitty,
    }
}

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
        let mut img_path = PathBuf::from(common::load_env("TUCK_IMG_PATH"));
        img_path.push(d.file_name);
        let mut gif = load_gif(img_path, d.dimension).expect("Failed to load gif");
        if let Some(range) = &d.frame_range {
            gif = gif.drain(range.clone()).collect();
        }
        (d, gif)
    });

    let endpoints =
        common::endpoints::tcp_from_env([EventType::MessageCreate, EventType::InteractionCreate])?;

    let plugin = TuckPlugin::connect_init(endpoints, async |rpc| {
        command::register::<Tuck>(rpc, None).await?;

        common::anyhow::Ok(TuckPlugin { tuck_gifs })
    })
    .await?;

    plugin.handle_events().await?;

    Ok(())
}

struct TuckPlugin<const GIF_COUNT: usize> {
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
    const ID: &'static str = "tuck";

    type RpcPolicy = HasRpc<true>;
    type EventsPolicy = HasEvents<true>;
}

impl<const GIF_COUNT: usize> HandleEvents for TuckPlugin<GIF_COUNT> {
    type Err = PluginError;

    async fn on_event(&self, rpc: rpc::ProtocolClient, event: Event) -> Result<(), Self::Err> {
        match event {
            Event::MessageCreate { message: _ } => {}
            Event::InteractionCreate { interaction } => {
                let Tuck {
                    target: user_id_to_tuck,
                    flavor,
                } = match Tuck::parse(&interaction) {
                    Some(Ok(tuck)) => tuck,
                    Some(Err(err)) => {
                        tracing::warn!("Invalid tuck interaction: {err}");
                        return Ok(());
                    }
                    None => return Ok(()),
                };
                let gif_idx = flavor.map(|flavor| flavor.value() as usize);

                let CommandInteraction {
                    id,
                    token,
                    channel_id,
                    user: author,
                    ..
                } = *interaction;

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
