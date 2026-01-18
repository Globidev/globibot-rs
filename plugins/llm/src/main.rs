#![feature(vec_deque_truncate_front)]

mod openrouter;
mod personality;

use axum::{
    Extension, Json,
    extract::{FromRef, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use axum_extra::extract::{PrivateCookieJar, cookie::Key};
use openrouter::{ContentPart, ImageContentPart, Message as LlmMessage, Role, TextContentPart};
use parking_lot::{Mutex, RwLock};
use tokio::net::ToSocketAddrs;

use std::{
    collections::{HashMap, VecDeque, hash_map::Entry},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use globibot_core::{
    events::{Event, EventType},
    plugin::{HandleEvents, HasEvents, HasRpc, Plugin},
    rpc,
    serenity::all::{
        Channel, ChannelId, CommandDataOptionValue, CommandId, CommandInteraction, Message, UserId,
    },
    storage::{DiscordProfile, DiscordSession, RedisStorage},
};
use itertools::Itertools;

use crate::personality::Personality;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let web_addr_listen = std::env::var("LLM_WEB_ADDR_LISTEN")?;
    let web_addr_advertise = std::env::var("LLM_WEB_ADDR_ADVERTISE")?.parse()?;
    let cookie_secret = std::env::var("COOKIE_SECRET")?;

    let guild_id = std::env::var("LLM_INSTALL_COMMAND_GUILD_ID")?.parse()?;
    let desired_command: serde_json::Value =
        serde_json::from_str(include_str!("../llm-slash-command.json"))?;

    let endpoints =
        common::endpoints::tcp_from_env([EventType::MessageCreate, EventType::InteractionCreate])?;

    let plugin = LlmPlugin::connect_init(endpoints, async |rpc| {
        let command = rpc
            .upsert_guild_command(rpc::context::current(), guild_id, desired_command)
            .await??;

        rpc.register_web_api(rpc::context::current(), web_addr_advertise)
            .await??;

        LlmPlugin::from_env(command.id)
    })
    .await?;

    let llm_plugin = Arc::clone(&plugin.inner);
    let storage = RedisStorage::from_env().await?;
    let web_server = WebServer::new(storage, &cookie_secret, llm_plugin);

    tokio::select! {
        _ = web_server.serve(web_addr_listen) => {
            tracing::info!("Web server has shut down");
        },
        _ = plugin.handle_events() => {
            tracing::info!("Event handler has shut down");
        }
    }

    Ok(())
}

#[derive(Debug, Clone, FromRef)]
struct WebServer {
    storage: RedisStorage,
    cookie_key: Key,
    llm_plugin: Arc<LlmPlugin>,
}

impl WebServer {
    pub fn new(storage: RedisStorage, cookie_secret: &str, llm_plugin: Arc<LlmPlugin>) -> Self {
        Self {
            storage,
            cookie_key: Key::from(cookie_secret.as_bytes()),
            llm_plugin,
        }
    }

    pub async fn serve(self, addr: impl ToSocketAddrs) -> std::io::Result<()> {
        let app = axum::Router::new() //
            .route("/settings", axum::routing::get(llm_settings))
            .route("/settings", axum::routing::post(update_llm_settings))
            .with_state(self.clone())
            .layer(middleware::from_fn_with_state(self, discord_auth));

        let listener = tokio::net::TcpListener::bind(addr).await?;
        axum::serve(listener, app).await?;

        Ok(())
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct WebLLMSettings {
    model: String,
    context_window_size: usize,

    #[serde(skip_deserializing)]
    context_windows_by_channel: Vec<ChannelContextWindow>,

    #[serde(skip_deserializing)]
    allowed_to_edit: Option<bool>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ChannelContextWindow {
    channel: Channel,
    size: usize,
}

async fn llm_settings(
    State(llm_plugin): State<Arc<LlmPlugin>>,
    Extension(profile): Extension<DiscordProfile>,
) -> impl IntoResponse {
    let model = llm_plugin.llm_model.read().clone();
    let context_window_size = llm_plugin.context_window_size.load(Ordering::Relaxed);
    let context_windows_by_channel = {
        let contexts_by_channel = llm_plugin.contexts_by_channel.lock();
        contexts_by_channel
            .values()
            .map(|ctx| ChannelContextWindow {
                channel: ctx.channel.clone(),
                size: ctx.messages.len(),
            })
            .collect()
    };

    let settings = WebLLMSettings {
        model,
        context_window_size,
        context_windows_by_channel,
        allowed_to_edit: Some(llm_plugin.admin_id == profile.user_id),
    };

    Json(settings)
}

async fn update_llm_settings(
    State(llm_plugin): State<Arc<LlmPlugin>>,
    Extension(profile): Extension<DiscordProfile>,
    Json(settings): Json<WebLLMSettings>,
) -> impl IntoResponse {
    if llm_plugin.admin_id != profile.user_id {
        return (
            StatusCode::FORBIDDEN,
            "You do not have permission to update LLM settings",
        )
            .into_response();
    }

    *llm_plugin.llm_model.write() = settings.model;
    llm_plugin
        .context_window_size
        .store(settings.context_window_size, Ordering::Relaxed);

    llm_settings(State(llm_plugin), Extension(profile))
        .await
        .into_response()
}

async fn discord_auth(
    State(mut storage): State<RedisStorage>,
    jar: PrivateCookieJar,
    mut req: Request,
    next: Next,
) -> Response {
    let Some(cookie) = jar.get("discord_session") else {
        return (
            StatusCode::UNAUTHORIZED,
            "You need to be logged in to access this resource",
        )
            .into_response();
    };
    let session_id = cookie.value();
    let profile = match storage.get::<DiscordProfile>(session_id).await {
        Ok(profile) => profile,
        Err(err) => {
            tracing::error!("Failed to get Discord session from storage: {err:?}");
            return (StatusCode::UNAUTHORIZED, "Invalid session").into_response();
        }
    };

    req.extensions_mut().insert(profile);

    next.run(req).await
}

#[derive(Debug)]
struct LlmPlugin {
    bot_id: UserId,
    admin_id: UserId,
    command_id: CommandId,

    llm_client: openrouter::Client,

    llm_model: RwLock<String>,
    personality: RwLock<Personality>,
    contexts_by_channel: Mutex<HashMap<ChannelId, ChannelContext>>,

    context_window_size: AtomicUsize,
}

#[derive(Debug)]
struct ChannelContext {
    channel: Channel,
    messages: VecDeque<openrouter::Message>,
}

impl LlmPlugin {
    fn from_env(command_id: CommandId) -> anyhow::Result<Self> {
        const DEFAULT_CONTEXT_WINDOW_SIZE: usize = 1_000;

        let bot_id = std::env::var("DISCORD_BOT_ID")?.parse()?;
        let admin_id = std::env::var("LLM_ADMIN_USER_ID")?.parse()?;
        let model = std::env::var("LLM_DEFAULT_MODEL_ID")?;
        let llm_client = openrouter::Client::from_env()?;

        Ok(LlmPlugin {
            bot_id,
            admin_id,
            llm_client,
            llm_model: RwLock::new(model),
            personality: <_>::default(),
            contexts_by_channel: <_>::default(),
            command_id,
            context_window_size: AtomicUsize::new(DEFAULT_CONTEXT_WINDOW_SIZE),
        })
    }

    async fn answer_message(
        &self,
        rpc: rpc::ProtocolClient,
        message: &Message,
        user_llm_message: LlmMessage,
    ) -> anyhow::Result<()> {
        let ctx = rpc::context::current();

        let mut parts = self.context_for_channel(message.channel_id);
        parts.push(user_llm_message.clone());

        let typing = rpc.start_typing(ctx, message.channel_id).await??;
        let completion =
            self.llm_client
                .complete(&self.llm_model.read(), *self.personality.read(), parts);
        let completion_res = completion.await;
        rpc.stop_typing(ctx, typing).await??;
        self.register_message(rpc.clone(), message, user_llm_message)
            .await?;
        if let Ok(answer) = completion_res {
            rpc.send_reply(ctx, message.channel_id, answer.clone(), message.id)
                .await??;

            let bot_llm_message = LlmMessage {
                role: Role::Assistant,
                content: vec![ContentPart::Text(TextContentPart {
                    kind: "text",
                    text: answer,
                })],
            };
            self.register_message(rpc, message, bot_llm_message).await?;
        } else {
            tracing::error!("Failed to get LLM completion: {:?}", completion_res.err());
            rpc.send_reply(
                ctx,
                message.channel_id,
                "Sorry, I lost my train of thought.".to_string(),
                message.id,
            )
            .await??;
        }

        Ok(())
    }

    #[expect(clippy::await_holding_lock)] // 🔶 Fetch channel after releasing the lock
    async fn register_message(
        &self,
        rpc: rpc::ProtocolClient,
        message: &Message,
        llm_message: LlmMessage,
    ) -> anyhow::Result<()> {
        let mut contexts_by_channel = self.contexts_by_channel.lock();
        let context = match contexts_by_channel.entry(message.channel_id) {
            Entry::Occupied(occupied_entry) => occupied_entry.into_mut(),
            Entry::Vacant(vacant_entry) => {
                let channel = rpc
                    .get_channel(rpc::context::current(), message.channel_id)
                    .await??;
                vacant_entry.insert(ChannelContext {
                    channel,
                    messages: VecDeque::new(),
                })
            }
        };

        let messages = &mut context.messages;
        messages.push_back(llm_message);
        let max_size = self.context_window_size.load(Ordering::Relaxed);
        if messages.len() > max_size {
            messages.truncate_front(max_size);
        }

        Ok(())
    }

    fn context_for_channel(&self, chan_id: ChannelId) -> Vec<openrouter::Message> {
        let contexts_by_channel = self.contexts_by_channel.lock();
        let context = contexts_by_channel.get(&chan_id);

        if let Some(context) = context {
            context.messages.iter().cloned().collect_vec()
        } else {
            vec![]
        }
    }

    async fn show_model(
        &self,
        rpc: rpc::ProtocolClient,
        interaction: &CommandInteraction,
    ) -> anyhow::Result<()> {
        rpc.create_interaction_response(
            rpc::context::current(),
            interaction.id,
            interaction.token.clone(),
            serde_json::json!({
                "type": 4,
                "data": {
                    "content": format!(
                        "Current model is set to `{}`",
                        self.llm_model.read()
                    )
                }
            }),
        )
        .await??;
        Ok(())
    }

    async fn set_model(
        &self,
        rpc: rpc::ProtocolClient,
        interaction: &CommandInteraction,
        value: &CommandDataOptionValue,
    ) -> anyhow::Result<()> {
        if interaction.user.id != self.admin_id {
            rpc.create_interaction_response(
                rpc::context::current(),
                interaction.id,
                interaction.token.clone(),
                serde_json::json!({
                    "type": 4,
                    "data": {
                        "content": format!("You do not have permission to change the model. ask <@{}>", self.admin_id)
                    }
                }),
            )
            .await??;
            return Ok(());
        }

        if let CommandDataOptionValue::SubCommand(opts) = value
            && let Some(opt) = opts.first()
            && opt.name == "model"
            && let Some(new_model) = opt.value.as_str()
        {
            *self.llm_model.write() = new_model.trim().to_string();
            rpc.create_interaction_response(
                rpc::context::current(),
                interaction.id,
                interaction.token.clone(),
                serde_json::json!({
                    "type": 4,
                    "data": {
                        "content": format!("Model changed to `{new_model}`")
                    }
                }),
            )
            .await??;
        }

        Ok(())
    }

    async fn show_personality(
        &self,
        rpc: rpc::ProtocolClient,
        interaction: &CommandInteraction,
    ) -> anyhow::Result<()> {
        rpc.create_interaction_response(
            rpc::context::current(),
            interaction.id,
            interaction.token.clone(),
            serde_json::json!({
                "type": 4,
                "data": {
                    "content": format!(
                        "Current personality is set to `{}`",
                        self.personality.read()
                    )
                }
            }),
        )
        .await??;
        Ok(())
    }

    async fn set_personality(
        &self,
        rpc: rpc::ProtocolClient,
        interaction: &CommandInteraction,
        value: &CommandDataOptionValue,
    ) -> anyhow::Result<()> {
        if let CommandDataOptionValue::SubCommand(opts) = value
            && let Some(opt) = opts.first()
            && opt.name == "personality"
            && let Some(new_personality) = opt.value.as_str()
        {
            let Ok(new_personality) = Personality::try_from(new_personality) else {
                rpc.create_interaction_response(
                    rpc::context::current(),
                    interaction.id,
                    interaction.token.clone(),
                    serde_json::json!({
                        "type": 4,
                        "data": {
                            "content": format!("Unknown personality `{new_personality}`")
                        }
                    }),
                )
                .await??;
                return Ok(());
            };

            *self.personality.write() = new_personality;

            rpc.create_interaction_response(
                rpc::context::current(),
                interaction.id,
                interaction.token.clone(),
                serde_json::json!({
                    "type": 4,
                    "data": {
                        "content": format!("Personality changed to `{new_personality}`")
                    }
                }),
            )
            .await??;
        }

        Ok(())
    }
}

impl Plugin for LlmPlugin {
    const ID: &'static str = "llm";

    type RpcPolicy = HasRpc<true>;
    type EventsPolicy = HasEvents<true>;
}

impl HandleEvents for LlmPlugin {
    type Err = anyhow::Error;

    async fn on_event(&self, rpc: rpc::ProtocolClient, event: Event) -> Result<(), Self::Err> {
        match event {
            Event::InteractionCreate { interaction } if interaction.data.id == self.command_id => {
                // dbg!(&interaction);
                use CommandDataOptionValue::*;
                let Some(sub_cmd) = interaction.data.options.first() else {
                    return Ok(());
                };

                match (sub_cmd.name.as_str(), &sub_cmd.value) {
                    ("model", SubCommandGroup(opts)) => match opts.first() {
                        Some(opt) if opt.name == "show" => {
                            self.show_model(rpc, &interaction).await?
                        }
                        Some(opt) if opt.name == "set" => {
                            self.set_model(rpc, &interaction, &opt.value).await?
                        }
                        _ => {}
                    },
                    ("personality", SubCommandGroup(opts)) => match opts.first() {
                        Some(opt) if opt.name == "show" => {
                            self.show_personality(rpc, &interaction).await?
                        }
                        Some(opt) if opt.name == "set" => {
                            self.set_personality(rpc, &interaction, &opt.value).await?
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }

            Event::MessageCreate { message } if !message.author.bot => {
                let user_name = message
                    .member
                    .as_ref()
                    .and_then(|m| m.nick.as_deref())
                    .unwrap_or_else(|| &message.author.name);
                let user_id = message.author.id.get();

                let content_safe = rpc
                    .content_safe(
                        rpc::context::current(),
                        message.content.clone(),
                        message.guild_id,
                    )
                    .await??
                    .replace("@rust-bot", "@globibot");

                let user_llm_message = {
                    let mut content = vec![ContentPart::Text(TextContentPart {
                        kind: "text",
                        text: format!("{user_name} (<@{user_id}>): {content_safe}"),
                    })];

                    if false {
                        content.extend(message.attachments.iter().filter_map(|att| {
                            let _dims = att.dimensions()?;
                            Some(ContentPart::Image(ImageContentPart {
                                kind: "image_url",
                                image_url: openrouter::ImageUrl {
                                    url: att.url.clone(),
                                },
                            }))
                        }));
                    }

                    LlmMessage {
                        role: Role::User,
                        content,
                    }
                };

                if message.mentions_user_id(self.bot_id) {
                    self.answer_message(rpc, &message, user_llm_message).await?;
                } else {
                    self.register_message(rpc, &message, user_llm_message)
                        .await?;
                }
            }

            _ => {}
        }

        Ok(())
    }
}

// bot_command! {
//     enum LLMCommand {
//         #[subcommand]
//         Model(ModelCommand),

//         #[subcommand]
//         Personality(PersonalityCommand),
//     }

// }
// enum ModelCommand {
//     Show,
//     Set { model: String },
// }

// enum PersonalityCommand {
//     Show,
//     Set { personality: String },
// }

// macro_rules! bot_command {
//     (enum $name: ident { $( $tt:tt )* }) => {
//         bot_command!(@gen_enum $name $( $tt )* );
//     };

//     (@gen_enum $enum_name: ident $(#[$meta: meta])? $variant_name: ident ( $($ty: ty),* ) $( $tt: tt )*) => {
//         enum $enum_name {

//         }
//     }
// }
// use bot_command;
