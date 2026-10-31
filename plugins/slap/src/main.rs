use common::{
    anyhow,
    imageops::{self, Avatar, GifBuilder},
};
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
use rand::Rng;
use std::time::Instant;

bot_command! {
    #[slash(scope = guild, name = "slap", description = "Slap someone you don't like")]
    struct Slap {
        #[option(name = "target", description = "The user to slap")]
        target: UserId,
        #[option(name = "flavor", description = "The way you want to slap them")]
        flavor: Option<SlapFlavor>,
    }
}

bot_choices! {
    /// Values are indices into the slap scenarios
    enum SlapFlavor: i64 {
        #[choice(name = "HD static slap", value = 0)]
        Static,
        #[choice(name = "Animated slap", value = 1)]
        Animated,
    }
}

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

    let endpoints =
        common::endpoints::tcp_from_env([EventType::MessageCreate, EventType::InteractionCreate])?;

    let slap_scenarios = vec![
        scenario::static_slap::load_scenario()?,
        scenario::animated_slap::load_scenario()?,
    ];

    let plugin = SlapPlugin::connect_init(endpoints, async |rpc| {
        command::register::<Slap>(rpc, Some(guild_id)).await?;

        anyhow::Ok(SlapPlugin { slap_scenarios })
    })
    .await?;

    plugin.handle_events().await?;

    Ok(())
}

struct SlapPlugin {
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
    const ID: &'static str = "slap";

    type RpcPolicy = HasRpc<true>;
    type EventsPolicy = HasEvents<true>;
}

impl HandleEvents for SlapPlugin {
    type Err = anyhow::Error;

    async fn on_event(&self, rpc: rpc::ProtocolClient, event: Event) -> Result<(), Self::Err> {
        match event {
            Event::MessageCreate { message: _ } => {}
            Event::InteractionCreate { interaction } => {
                let Slap {
                    target: user_id_to_slap,
                    flavor,
                } = match Slap::parse(&interaction) {
                    Some(Ok(slap)) => slap,
                    Some(Err(err)) => {
                        tracing::warn!("Invalid slap interaction: {err}");
                        return Ok(());
                    }
                    None => return Ok(()),
                };
                let descriptor_idx = flavor.map(|flavor| flavor.value() as usize);

                let CommandInteraction {
                    id,
                    token,
                    channel_id,
                    user: author,
                    ..
                } = *interaction;

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
