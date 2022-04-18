#![feature(type_alias_impl_trait)]

use std::{
    error::Error,
    sync::Arc,
    time::{Duration, Instant},
};

use globibot_core::{
    events::{Event, EventType},
    plugin::{Endpoints, HandleEvents, HasEvents, HasRpc, Plugin},
    rpc::{self, context::current as rpc_context},
    serenity::{
        model::{
            id::UserId,
            interactions::application_command::{
                ApplicationCommandInteraction, ApplicationCommandInteractionDataOptionValue,
            },
            misc::Mentionable,
        },
        utils::parse_username,
    },
    transport::Tcp,
};

use futures::{lock::Mutex, Future};
use globibot_plugin_rateme::{load_rating_images, paste_rates_on_avatar, rate};
use rand::{Rng, SeedableRng};
use rate::Rate;

type PluginError = Box<dyn Error + Send + Sync>;

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let img_path = load_env("RATEME_IMG_PATH");
    let rating_images_small =
        load_rating_images(&img_path, (25, 25)).expect("Failed to load rating images");
    let rating_images_medium =
        load_rating_images(&img_path, (50, 50)).expect("Failed to load rating images");

    let plugin = RatemePlugin {
        rng: Mutex::new(rand::rngs::StdRng::from_entropy()),
        rating_images_small,
        rating_images_medium,
        command_id: load_env("RATEME_COMMAND_ID")
            .parse()
            .expect("Invalid command id"),
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

struct RatemePlugin<R: Rng> {
    rng: Mutex<R>,
    rating_images_small: Vec<image::DynamicImage>,
    rating_images_medium: Vec<image::DynamicImage>,
    command_id: u64,
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
        let avatar =
            globibot_plugin_common::imageops::load_avatar(avatar_url, (75_u32, 75_u32)).await?;

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

    type Future = impl Future<Output = Result<(), Self::Err>>;

    fn on_event(self: Arc<Self>, rpc: rpc::ProtocolClient, event: Event) -> Self::Future {
        async move {
            match event {
                Event::MessageCreate { message: _ } => {}
                Event::InteractionCreate {
                    interaction:
                        ApplicationCommandInteraction {
                            id,
                            data: command,
                            channel_id,
                            token,
                            member: Some(author),
                            ..
                        },
                } if command.id == self.command_id => {
                    let (target, user_to_rate) = match command
                        .options
                        .first()
                        .and_then(|opt| opt.resolved.as_ref())
                    {
                        Some(ApplicationCommandInteractionDataOptionValue::User(u, _)) => {
                            (RateTarget::User(u.id), u)
                        }
                        _ => (RateTarget::Me, &author.user),
                    };

                    let rate = self.rng.lock().await.gen::<Rate>();

                    let whose_face = match target {
                        RateTarget::User(user_id) => format!("{}'s", user_id.mention()),
                        RateTarget::Me => "your".to_owned(),
                    };

                    let avatar_url = user_to_rate
                        .avatar_url()
                        .unwrap_or_else(|| user_to_rate.default_avatar_url());

                    let generate_gif = tokio::spawn({
                        let plugin = Arc::clone(&self);
                        async move { plugin.generate_rating_gif(rate, &avatar_url).await }
                    });

                    rpc.create_interaction_response(
                        rpc_context(),
                        id.0,
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

                    let gif = match generate_gif.await? {
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
    let first_mention = parse_username(first_word)?;

    if first_mention != 168304788465909760 {
        return None;
    }

    let _rate_word = words.find(|&w| w == "rate")?;
    let following_rate = words.next()?;
    if following_rate == "me" {
        Some(RateTarget::Me)
    } else {
        Some(RateTarget::User(UserId(parse_username(following_rate)?)))
    }
}
