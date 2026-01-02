use std::{
    error::Error,
    time::{Duration, Instant},
};

use globibot_core::{
    events::{Event, EventType},
    plugin::{HandleEvents, HasEvents, HasRpc, Plugin},
    rpc::{self, context::current as rpc_context},
    serenity::{
        all::CommandId,
        model::{
            application::{CommandDataOptionValue, CommandInteraction},
            id::UserId,
            mention::Mentionable,
        },
        utils::parse_user_mention,
    },
};

use globibot_plugin_rateme::{load_rating_images, paste_rates_on_avatar, rate};
use rand::{Rng, SeedableRng};
use rate::Rate;

type PluginError = Box<dyn Error + Send + Sync>;

#[tokio::main]
async fn main() -> common::anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let img_path = load_env("RATEME_IMG_PATH");
    let rating_images_small = load_rating_images(&img_path, (25, 25))?;
    let rating_images_medium = load_rating_images(&img_path, (50, 50))?;

    let endpoints =
        common::endpoints::tcp_from_env([EventType::MessageCreate, EventType::InteractionCreate])?;

    let desired_command: serde_json::Value =
        serde_json::from_str(include_str!("../rateme-slash-command.json"))?;

    let plugin = RatemePlugin::connect_init(endpoints, async |rpc| {
        let command = rpc
            .upsert_global_command(rpc_context(), desired_command)
            .await??;

        common::anyhow::Ok(RatemePlugin {
            rng: rand::rngs::StdRng::from_os_rng().into(),
            rating_images_small,
            rating_images_medium,
            command_id: command.id,
        })
    })
    .await?;

    plugin.handle_events().await?;

    Ok(())
}

fn load_env(key: &str) -> String {
    std::env::var(key)
        .unwrap_or_else(|why| panic!("Failed to load environment variable '{}': {}", key, why))
}

struct RatemePlugin<R: Rng> {
    rng: parking_lot::Mutex<R>,
    rating_images_small: Vec<common::image::DynamicImage>,
    rating_images_medium: Vec<common::image::DynamicImage>,
    command_id: CommandId,
}

impl<R: Rng> Plugin for RatemePlugin<R> {
    const ID: &'static str = "rateme";

    type RpcPolicy = HasRpc<true>;
    type EventsPolicy = HasEvents<true>;
}

impl<R: Rng> RatemePlugin<R> {
    async fn generate_rating_gif(
        &self,
        rate: Rate,
        avatar_url: &str,
    ) -> Result<Vec<u8>, PluginError> {
        let avatar = common::imageops::load_avatar(avatar_url, (75_u32, 75_u32)).await?;

        let t0 = Instant::now();
        let gif = tokio::task::spawn_blocking({
            let small_frames = self.rating_images_small.clone();
            let final_frame = self.rating_images_medium[rate as usize].clone();
            move || paste_rates_on_avatar(avatar, small_frames, &final_frame)
        })
        .await??;
        tracing::info!(
            "Generated image of {}b in {}ms",
            gif.len(),
            t0.elapsed().as_millis()
        );

        Ok(gif)
    }
}

impl<R: Rng + Send + 'static> HandleEvents for RatemePlugin<R> {
    type Err = PluginError;

    async fn on_event(&self, rpc: rpc::ProtocolClient, event: Event) -> Result<(), Self::Err> {
        match event {
            Event::MessageCreate { message: _ } => {}
            Event::InteractionCreate { interaction } if interaction.data.id == self.command_id => {
                let CommandInteraction {
                    id,
                    data: command,
                    token,
                    channel_id,
                    user: author,
                    ..
                } = *interaction;

                let (target, user_to_rate) = match command.options.first().map(|opt| &opt.value) {
                    Some(&CommandDataOptionValue::User(user_id)) => {
                        let user = rpc.get_user(rpc_context(), user_id).await??;
                        (RateTarget::User(user_id), user)
                    }
                    _ => (RateTarget::Me, author.clone()),
                };

                let rate = self.rng.lock().random::<Rate>();

                let whose_face = match target {
                    RateTarget::User(user_id) => format!("{}'s", user_id.mention()),
                    RateTarget::Me => "your".to_owned(),
                };

                let avatar_url = user_to_rate
                    .avatar_url()
                    .unwrap_or_else(|| user_to_rate.default_avatar_url());

                rpc.create_interaction_response(
                    rpc_context(),
                    id,
                    token.clone(),
                    serde_json::json!({
                        "type": 4,
                        "data": {
                            "content": format!(
                                "{} hold on, I'm computing {} face…",
                                author.mention(),
                                whose_face
                            ),
                        }
                    }),
                )
                .await??;

                let gif = match self.generate_rating_gif(rate, &avatar_url).await {
                    Ok(gif) => gif,
                    Err(e) => {
                        tracing::error!("failed to generate gif: {}", &e);
                        rpc.edit_interaction_response(
                            rpc_context(),
                            token,
                            serde_json::json!({ "content":
                                format!(
                                    "I am unable to accurately compute the rating for some \
                                    reason\nbut {} look like an __HB{}__ {}",
                                    match target {
                                        RateTarget::User(_) => "they",
                                        RateTarget::Me => "you",
                                    },
                                    rate as u8,
                                    rate.emote()
                                ),
                            }),
                        )
                        .await??;
                        return Err(e);
                    }
                };

                let p2message = rpc
                    .send_file(rpc_context(), channel_id, gif, "rate.gif".to_owned())
                    .await??;

                tokio::time::sleep(Duration::from_secs(19)).await;
                let p2content = format!(
                    "Looks like {} an __HB{}__ {}",
                    match target {
                        RateTarget::User(user_id) => format!("{} is", user_id.mention()),
                        RateTarget::Me => "you are".to_owned(),
                    },
                    rate as u8,
                    rate.emote()
                );
                rpc.edit_message(rpc_context(), p2message, p2content)
                    .await??;
            }
            _ => {}
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy)]
enum RateTarget {
    User(UserId),
    Me,
}

#[allow(unused)]
fn parse_rate_command(content: &str) -> Option<RateTarget> {
    let mut words = content.split_whitespace();
    let first_word = words.next()?;
    let first_mention = parse_user_mention(first_word)?;

    if first_mention != 168304788465909760 {
        return None;
    }

    let _rate_word = words.find(|&w| w == "rate")?;
    let following_rate = words.next()?;
    if following_rate == "me" {
        Some(RateTarget::Me)
    } else {
        Some(RateTarget::User(parse_user_mention(following_rate)?))
    }
}
