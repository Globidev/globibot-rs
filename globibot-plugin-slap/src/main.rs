#![feature(type_alias_impl_trait)]

use std::{
    convert::TryInto,
    error::Error,
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
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
use globibot_plugin_common::imageops::{load_avatar, Avatar, GifBuilder};
use image::RgbaImage;
use rand::Rng;

type PluginError = Box<dyn Error + Send + Sync>;

#[derive(Debug, Clone)]
struct SlapDescriptor {
    dim: (u16, u16),
    avatar_dim: (u16, u16),
    slapper_positions: Vec<(u32, u32)>,
    slapped_positions: Vec<(u32, u32)>,
    frames: Vec<RgbaImage>,
}

const ROCK_POS: [(u32, u32); 47] = [
    (139, 138),
    (139, 138),
    (144, 136),
    (144, 137),
    (144, 139),
    (146, 137),
    (146, 139),
    (148, 136),
    (148, 137),
    (154, 138),
    (153, 138),
    (154, 138),
    (154, 140),
    (155, 138),
    (157, 139),
    (153, 140),
    (156, 138),
    (155, 138),
    (157, 136),
    (156, 136),
    (156, 136),
    (152, 136),
    (138, 138),
    (121, 150),
    (110, 164),
    (99, 166),
    (100, 166),
    (100, 162),
    (103, 160),
    (106, 158),
    (110, 152),
    (112, 150),
    (117, 158),
    (121, 148),
    (113, 147),
    (113, 147),
    (109, 149),
    (102, 151),
    (97, 152),
    (96, 153),
    (95, 153),
    (95, 153),
    (95, 150),
    (96, 147),
    (100, 147),
    (105, 146),
    (110, 146),
];

const SMITH_POS: [(u32, u32); 47] = [
    (278, 98),
    (276, 98),
    (274, 101),
    (271, 107),
    (270, 113),
    (268, 117),
    (264, 118),
    (261, 116),
    (256, 113),
    (251, 115),
    (248, 116),
    (241, 118),
    (233, 122),
    (228, 124),
    (221, 123),
    (219, 120),
    (216, 119),
    (216, 118),
    (213, 117),
    (212, 119),
    (211, 115),
    (212, 112),
    (214, 110),
    (217, 114),
    (219, 115),
    (215, 113),
    (212, 115),
    (210, 118),
    (212, 120),
    (216, 122),
    (220, 122),
    (224, 125),
    (232, 124),
    (233, 124),
    (239, 126),
    (243, 124),
    (244, 123),
    (248, 126),
    (250, 128),
    (254, 129),
    (260, 127),
    (269, 123),
    (272, 124),
    (274, 123),
    (278, 118),
    (281, 116),
    (286, 119),
];

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let img_path = |file_name| {
        let mut path = PathBuf::from(load_env("SLAP_IMG_PATH"));
        path.push(file_name);
        path
    };

    let static_image = image::open(img_path("slap-hd.png"))
        .expect("Failed to load static image")
        .into_rgba8();

    let animated_image_frames = globibot_plugin_common::imageops::load_gif(
        img_path("slap-animated.gif"),
        (480_u32, 480_u32),
    )
    .expect("Failed to load gif");

    let slap_descriptors = vec![
        SlapDescriptor {
            dim: (2560, 1707),
            avatar_dim: (300, 300),
            slapper_positions: vec![(1631, 207)],
            slapped_positions: vec![(751, 266)],
            frames: vec![static_image],
        },
        SlapDescriptor {
            dim: (480, 480),
            avatar_dim: (50, 50),
            slapper_positions: SMITH_POS.to_vec(),
            slapped_positions: ROCK_POS.to_vec(),
            frames: animated_image_frames,
        },
    ];

    let plugin = SlapPlugin {
        command_id: load_env("SLAP_COMMAND_ID")
            .parse()
            .expect("Malformed command id"),
        slap_descriptors,
        command_updated: Arc::new(AtomicBool::new(false)),
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

struct SlapPlugin {
    command_id: u64,
    slap_descriptors: Vec<SlapDescriptor>,
    command_updated: Arc<AtomicBool>,
}

impl SlapPlugin {
    async fn generate_slapping_gif(
        &self,
        slapper_avatar_url: &str,
        slapped_avatar_url: &str,
        descriptor_idx: Option<usize>,
    ) -> Result<Vec<u8>, PluginError> {
        let idx = descriptor_idx
            .unwrap_or_else(|| rand::thread_rng().gen_range(0..self.slap_descriptors.len()));
        let desc = self.slap_descriptors[idx].clone();

        let (slapper_avatar, slapped_avatar) = futures::try_join!(
            load_avatar(slapper_avatar_url, desc.avatar_dim),
            load_avatar(slapped_avatar_url, desc.avatar_dim),
        )?;

        let t0 = Instant::now();
        let gif = tokio::task::spawn_blocking(move || {
            let mut builder = GifBuilder::from_background_frames(desc.frames, desc.dim);

            match slapper_avatar {
                Avatar::Animated(frames) => builder.overlay(&frames, &desc.slapper_positions),
                Avatar::Fixed(frame) => builder.overlay(&[frame], &desc.slapper_positions),
            };

            match slapped_avatar {
                Avatar::Animated(frames) => builder.overlay(&frames, &desc.slapped_positions),
                Avatar::Fixed(frame) => builder.overlay(&[frame], &desc.slapped_positions),
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
    type Err = PluginError;
    type Future = impl std::future::Future<Output = Result<(), Self::Err>>;

    fn on_event(self: Arc<Self>, rpc: rpc::ProtocolClient, event: Event) -> Self::Future {
        async move {
            if !self.command_updated.load(Ordering::Relaxed)
                && std::env::args().any(|x| x == "--update-slash-cmd")
            {
                self.command_updated.store(true, Ordering::Relaxed);
                let application = rpc
                    .create_global_command(
                        rpc_context(),
                        // self.command_id,
                        serde_json::from_str(include_str!("../slap-slash-command.json")).unwrap(),
                    )
                    .await??;
                // let application = rpc
                //     .create_guild_command(
                //         rpc_context(),
                //         GuildId(143032611814637568),
                //         serde_json::from_str(include_str!("../slap-slash-command.json")).unwrap(),
                //     )
                //     .await??;

                println!("UPDATED COMMAND ID: {}", application.id);
            }

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
                    let user_to_slap = match command
                        .options
                        .iter()
                        .find(|opt| opt.name == "target")
                        .and_then(|opt| opt.resolved.as_ref())
                    {
                        Some(ApplicationCommandInteractionDataOptionValue::User(u, _)) => u.clone(),
                        _ => return Ok(()),
                    };

                    let descriptor_idx = match command
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

                    let slapper_avatar_url = author
                        .avatar_url()
                        .unwrap_or_else(|| author.default_avatar_url());

                    let slapped_avatar_url = user_to_slap
                        .avatar_url()
                        .unwrap_or_else(|| user_to_slap.default_avatar_url());

                    let generate_gif = tokio::spawn({
                        let plugin = Arc::clone(&self);
                        async move {
                            plugin
                                .generate_slapping_gif(
                                    &slapper_avatar_url,
                                    &slapped_avatar_url,
                                    descriptor_idx,
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
                                    "{} walks angrily towards {}",
                                    author.mention(), user_to_slap.mention()
                                )
                            }
                        }),
                    )
                    .await??;

                    let gif = generate_gif.await??;
                    tracing::info!("Sending gif of {} bytes", gif.len());

                    rpc.send_file(rpc_context(), channel_id, gif, "slap.gif".to_owned())
                        .await??;
                }
                _ => (),
            }
            Ok(())
        }
    }
}
