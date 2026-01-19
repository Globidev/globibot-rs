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
        Channel, ChannelId, CommandDataOptionValue, CommandId, CommandInteraction, GuildId, Member,
        Message, UserId,
    },
    storage::{DiscordProfile, RedisStorage, StorageValue},
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

    let mut storage = RedisStorage::from_env().await?;

    let plugin = LlmPlugin::connect_init(endpoints, async |rpc| {
        let command = rpc
            .upsert_guild_command(rpc::context::current(), guild_id, desired_command)
            .await??;

        rpc.register_web_api(rpc::context::current(), web_addr_advertise)
            .await??;

        let members = rpc
            .list_guild_members(rpc::context::current(), guild_id)
            .await??;

        let lore_book = storage.get::<LoreBook>("latest").await.unwrap_or_default();

        LlmPlugin::from_env(guild_id, command.id, members, lore_book)
    })
    .await?;

    let llm_plugin = Arc::clone(&plugin.inner);

    let save_lore_regularly = {
        let plugin = Arc::clone(&plugin.inner);
        let mut storage = storage.clone();
        async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                let lore_book = plugin.lore_book.read().clone();
                if let Err(err) = storage.set("latest", &lore_book).await {
                    tracing::error!("Failed to save LLM lore book: {err:?}");
                }
            }
        }
    };

    let web_server = WebServer::new(storage, &cookie_secret, llm_plugin);

    tokio::select! {
        _ = web_server.serve(web_addr_listen) => {
            tracing::info!("Web server has shut down");
        },
        _ = plugin.handle_events() => {
            tracing::info!("Event handler has shut down");
        },
        _ = save_lore_regularly => {
            tracing::info!("Lore saving task has shut down");
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
            .route("/personality", axum::routing::get(llm_personality))
            .route("/personality", axum::routing::post(update_llm_personality))
            .route("/lore", axum::routing::get(llm_lore))
            .route("/lore/suggest", axum::routing::post(llm_lore_suggest))
            .route("/lore/vote", axum::routing::post(llm_lore_vote))
            .route("/lore/accept", axum::routing::post(llm_lore_accept))
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
    prompt: String,

    #[serde(skip_deserializing)]
    context_windows_by_channel: Vec<ChannelContextWindow>,

    #[serde(skip_deserializing)]
    allowed_to_edit: Option<bool>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct ChannelContextWindow {
    channel_name: String,
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
                channel_name: ctx
                    .channel
                    .clone()
                    .guild()
                    .map(|g| g.name.clone())
                    .unwrap_or_default(),
                size: ctx.messages.len(),
            })
            .collect()
    };

    let settings = WebLLMSettings {
        model,
        prompt: llm_plugin.system_prompt(),
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

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct LlmPersonality {
    personality: String,

    #[serde(skip_deserializing)]
    available_personalities: Vec<String>,

    #[serde(skip_deserializing)]
    prompt: String,
}

async fn llm_personality(State(llm_plugin): State<Arc<LlmPlugin>>) -> impl IntoResponse {
    let personality = *llm_plugin.personality.read();

    Json(LlmPersonality {
        personality: personality.to_string(),
        available_personalities: Personality::all_personalities()
            .map(|p| p.to_string())
            .collect(),
        prompt: personality.system_prompt(),
    })
}

async fn update_llm_personality(
    State(llm_plugin): State<Arc<LlmPlugin>>,
    Json(personality_req): Json<LlmPersonality>,
) -> impl IntoResponse {
    let new_personality = match personality_req.personality.as_str().try_into() {
        Ok(p) => p,
        Err(_) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Unknown personality `{}`", personality_req.personality),
            )
                .into_response();
        }
    };

    *llm_plugin.personality.write() = new_personality;

    llm_personality(State(llm_plugin)).await.into_response()
}

async fn llm_lore(State(llm_plugin): State<Arc<LlmPlugin>>) -> impl IntoResponse {
    let lore_book = llm_plugin.lore_book.read().clone();
    Json(lore_book)
}

#[derive(Debug, serde::Deserialize)]
struct LlmLoreSuggestionRequest {
    for_user_id: UserId,
    suggestion: String,
}

async fn llm_lore_suggest(
    State(llm_plugin): State<Arc<LlmPlugin>>,
    Extension(profile): Extension<DiscordProfile>,
    Json(suggestion_req): Json<LlmLoreSuggestionRequest>,
) -> impl IntoResponse {
    if profile.user_id == suggestion_req.for_user_id {
        return (
            StatusCode::BAD_REQUEST,
            "You cannot suggest lore changes for yourself",
        )
            .into_response();
    }

    let suggestion_text = suggestion_req.suggestion.trim();
    if suggestion_text.is_empty() {
        return (StatusCode::BAD_REQUEST, "Suggestion cannot be empty").into_response();
    }

    let mut lore_book = llm_plugin.lore_book.write();
    let user_lore = match lore_book.lore_by_user.get(&suggestion_req.for_user_id) {
        Some(lore) => lore,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "You must have lore to suggest changes to it",
            )
                .into_response();
        }
    };

    let suggestion = UserLoreSuggestion {
        member: user_lore.member.clone(),
        suggestion: suggestion_text.to_string(),
        suggestion_by: profile.clone(),
        votes_by_user_id: HashMap::new(),
    };

    let suggestions = lore_book
        .suggestions_by_user
        .entry(suggestion_req.for_user_id)
        .or_default();

    if let Some(existing_suggestion) = suggestions
        .iter_mut()
        .find(|s| s.suggestion_by.user_id == profile.user_id)
    {
        existing_suggestion.suggestion = suggestion_text.to_string();
    } else {
        suggestions.push(suggestion);
    }

    drop(lore_book);

    llm_lore(State(llm_plugin)).await.into_response()
}

#[derive(Debug, serde::Deserialize)]
struct LlmLoreVoteRequest {
    for_user_id: UserId,
    by_user_id: UserId,
    vote: SuggestionVote,
}

async fn llm_lore_vote(
    State(llm_plugin): State<Arc<LlmPlugin>>,
    Extension(profile): Extension<DiscordProfile>,
    Json(vote_req): Json<LlmLoreVoteRequest>,
) -> impl IntoResponse {
    if profile.user_id == vote_req.by_user_id {
        return (
            StatusCode::BAD_REQUEST,
            "You cannot vote on your own suggestions",
        )
            .into_response();
    }

    let mut lore_book = llm_plugin.lore_book.write();
    let suggestions = match lore_book.suggestions_by_user.get_mut(&vote_req.for_user_id) {
        Some(suggestions) => suggestions,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "No suggestions found for the specified user",
            )
                .into_response();
        }
    };

    let suggestion = match suggestions
        .iter_mut()
        .find(|s| s.suggestion_by.user_id == vote_req.by_user_id)
    {
        Some(suggestion) => suggestion,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "No suggestion found from the specified user",
            )
                .into_response();
        }
    };

    suggestion
        .votes_by_user_id
        .insert(profile.user_id, vote_req.vote);

    drop(lore_book);

    llm_lore(State(llm_plugin)).await.into_response()
}

#[derive(Debug, serde::Deserialize)]
struct LoreAcceptRequest {
    for_user_id: UserId,
    by_user_id: UserId,
}

async fn llm_lore_accept(
    State(llm_plugin): State<Arc<LlmPlugin>>,
    Extension(profile): Extension<DiscordProfile>,
    Json(accept_req): Json<LoreAcceptRequest>,
) -> impl IntoResponse {
    if llm_plugin.admin_id != profile.user_id {
        return (
            StatusCode::FORBIDDEN,
            "You do not have permission to accept lore suggestions",
        )
            .into_response();
    }

    let mut lore_book = llm_plugin.lore_book.write();
    let suggestions = match lore_book
        .suggestions_by_user
        .get_mut(&accept_req.for_user_id)
    {
        Some(suggestions) => suggestions,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "No suggestions found for the specified user",
            )
                .into_response();
        }
    };

    let suggestion_index = match suggestions
        .iter()
        .position(|s| s.suggestion_by.user_id == accept_req.by_user_id)
    {
        Some(index) => index,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                "No suggestion found from the specified user",
            )
                .into_response();
        }
    };

    let suggestion = suggestions.remove(suggestion_index);
    if let Some(user_lore) = lore_book.lore_by_user.get_mut(&accept_req.for_user_id) {
        user_lore.lore = suggestion.suggestion;
    }

    drop(lore_book);

    llm_lore(State(llm_plugin)).await.into_response()
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
    guild_id: GuildId,
    command_id: CommandId,

    llm_client: openrouter::Client,

    llm_model: RwLock<String>,
    personality: RwLock<Personality>,
    contexts_by_channel: Mutex<HashMap<ChannelId, ChannelContext>>,

    context_window_size: AtomicUsize,

    lore_book: RwLock<LoreBook>,
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
struct LoreBook {
    lore_by_user: HashMap<UserId, UserLore>,
    suggestions_by_user: HashMap<UserId, Vec<UserLoreSuggestion>>,
}

impl StorageValue for LoreBook {
    const REDIS_NS: &'static str = "lore_book";
}

impl LoreBook {
    fn to_prompt(&self) -> String {
        use std::fmt::Write;
        const PREMISE: &str = r#"
# "Facts" about people in the chat
Those are not necessarily true, but they are the "lore" of the chat that you should embrace
Use those facts sparingly to add flavor to your responses if appropriate.
Don't feel obligated to reference them in every response though.
"#;

        let mut prompt = format!("{PREMISE}\n");

        for user_lore in self.lore_by_user.values() {
            let lore = &user_lore.lore;
            if lore.is_empty() {
                continue;
            }
            let username = &user_lore.member.username;
            let user_id = user_lore.member.user_id;
            writeln!(prompt, "{username} (<@{user_id}>): {lore}").ok();
        }

        prompt
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct UserLore {
    member: DiscordProfile,
    lore: String,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct UserLoreSuggestion {
    member: DiscordProfile,
    suggestion: String,
    suggestion_by: DiscordProfile,
    votes_by_user_id: HashMap<UserId, SuggestionVote>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum SuggestionVote {
    Omegalul,
    Up,
    Down,
}

#[derive(Debug)]
struct ChannelContext {
    channel: Channel,
    messages: VecDeque<openrouter::Message>,
}

impl LlmPlugin {
    fn from_env(
        guild_id: GuildId,
        command_id: CommandId,
        members: Vec<Member>,
        mut lore_book: LoreBook,
    ) -> anyhow::Result<Self> {
        const DEFAULT_CONTEXT_WINDOW_SIZE: usize = 1_000;

        let bot_id = std::env::var("DISCORD_BOT_ID")?.parse()?;
        let admin_id = std::env::var("LLM_ADMIN_USER_ID")?.parse()?;
        let model = std::env::var("LLM_DEFAULT_MODEL_ID")?;
        let llm_client = openrouter::Client::from_env()?;

        for member in members {
            if member.user.bot {
                lore_book.lore_by_user.remove(&member.user.id);
                continue;
            }

            let profile = DiscordProfile {
                user_id: member.user.id,
                username: member.display_name().to_string(),
                avatar_url: Some(
                    member
                        .avatar_url()
                        .or(member.user.avatar_url())
                        .unwrap_or(member.user.default_avatar_url()),
                ),
            };

            lore_book
                .lore_by_user
                .entry(member.user.id)
                .and_modify(|lore| lore.member = profile.clone())
                .or_insert_with(|| UserLore {
                    member: profile,
                    lore: String::new(),
                });
        }

        Ok(LlmPlugin {
            bot_id,
            admin_id,
            guild_id,
            llm_client,
            llm_model: RwLock::new(model),
            personality: <_>::default(),
            contexts_by_channel: <_>::default(),
            command_id,
            context_window_size: AtomicUsize::new(DEFAULT_CONTEXT_WINDOW_SIZE),
            lore_book: RwLock::new(lore_book),
        })
    }

    fn system_prompt(&self) -> String {
        use std::fmt::Write;

        let personality = *self.personality.read();
        let mut system_prompt = personality.system_prompt();
        write!(system_prompt, "\n{}", self.lore_book.read().to_prompt()).ok();

        system_prompt
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
                .complete(&self.llm_model.read(), self.system_prompt(), parts);
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
                if message.guild_id.is_none_or(|gid| gid != self.guild_id) {
                    // Ignore messages outside of the configured guild
                    return Ok(());
                }

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
