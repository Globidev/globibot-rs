#![feature(type_alias_impl_trait)]

use common::{
    anyhow,
    imageops::{self, Avatar, GifBuilder},
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
use rand::Rng;
use std::{sync::Arc, time::Instant};

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
        pub slapper_positions: Vec<(u32, u32)>,
        pub slapped_positions: Vec<(u32, u32)>,
        pub frames: Vec<common::image::RgbaImage>,
    }
}

use scenario::SlapScenario;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let plugin = SlapPlugin {
        command_id: common::load_env("SLAP_COMMAND_ID").parse()?,
        slap_descriptors: vec![
            scenario::animated_slap::load_scenario()?,
            scenario::static_slap::load_scenario()?,
        ],
    };

    let events = [EventType::MessageCreate, EventType::InteractionCreate];

    let endpoints = Endpoints::new()
        .rpc(Tcp::new(common::load_env("RPC_ADDR")))
        .events(Tcp::new(common::load_env("SUBSCRIBER_ADDR")), events);

    let plugin = plugin.connect(endpoints).await?;
    // plugin
    //     .rpc
    //     .create_global_command(
    //         rpc_context(),
    //         // self.command_id,
    //         serde_json::from_str(include_str!("../slap-slash-command.json")).unwrap(),
    //     )
    //     .await??;
    plugin.handle_events().await?;

    Ok(())
}

struct SlapPlugin {
    command_id: u64,
    slap_descriptors: Vec<SlapScenario>,
}

impl SlapPlugin {
    async fn generate_slapping_gif(
        &self,
        slapper_avatar_url: &str,
        slapped_avatar_url: &str,
        descriptor_idx: Option<usize>,
    ) -> Result<Vec<u8>, anyhow::Error> {
        let idx = descriptor_idx
            .unwrap_or_else(|| rand::thread_rng().gen_range(0..self.slap_descriptors.len()));
        let desc = self.slap_descriptors[idx].clone();

        let (slapper_avatar, slapped_avatar) = futures::try_join!(
            imageops::load_avatar(slapper_avatar_url, desc.avatar_dim),
            imageops::load_avatar(slapped_avatar_url, desc.avatar_dim),
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
    type Err = anyhow::Error;
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
