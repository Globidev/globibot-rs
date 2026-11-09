mod chime_in;
mod commands;
mod context;
mod cooldown;
mod fun;
mod image_generation;
mod images;
mod message_search;
mod nightly_lore;
mod openrouter;
mod reminders;
mod settings;
mod tool;
mod tool_traces;
mod tools;
mod trivia;
mod usage;

use anyhow::Context;
use axum::{
    Extension, Json,
    extract::{FromRef, Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use axum_extra::extract::{PrivateCookieJar, cookie::Key};
use chime_in::{Candidate, ChimeIn};
use commands::{
    AddToLore, AddUserContext, Hype, Llm, LlmCommands, LoreTrivia, ModelCommand, PersonalityChoice,
    PersonalityCommand, Roast, Tldr,
};
use context::{ChannelContext, ContextMessage, SavedContext};
use cooldown::Cooldowns;
use fun::{Vibe, sanitize_llm_output};
use image_generation::{DailyCounts, ImageSettings};
use images::{Consent, MessageImages};
use openrouter::{
    ContentPart, Message as LlmMessage, Role, TextContentPart, ToolOutput, UrlCitation,
};
use parking_lot::{Mutex, RwLock};
use reminders::{REMINDERS_KEY, Reminders};
use settings::{LlmSettings, SETTINGS_KEY};
use tokio::net::ToSocketAddrs;
use tool::ToolCtx;
use tool_traces::{Outcome, TOOL_TRACES_KEY, ToolCallRecord, ToolTrace, ToolTraces};
use tools::LlmTool;
use trivia::TriviaRounds;
use usage::{USAGE_KEY, UsageKind, UsageStats};

use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    str::FromStr,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, Instant},
};

use globibot_core::{
    events::{Event, EventType},
    interaction::{CommandCtx, CommandInteractionExt},
    plugin::{HandleEvents, HasEvents, HasRpc, Plugin},
    rpc::{self, FileAttachment},
    serenity::all::{Channel, ChannelId, GuildId, Member, Message, ReactionType, RoleId, UserId},
    storage::{self, DiscordProfile, RedisStorage, StorageValue},
};
use itertools::Itertools;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let web_addr_listen = std::env::var("LLM_WEB_ADDR_LISTEN")?;
    let web_addr_advertise = std::env::var("LLM_WEB_ADDR_ADVERTISE")?.parse()?;
    let cookie_secret = std::env::var("COOKIE_SECRET")?;

    let guild_id = std::env::var("LLM_INSTALL_COMMAND_GUILD_ID")?.parse()?;

    let endpoints = common::endpoints::tcp_from_env([
        EventType::MessageCreate,
        EventType::MessageEdit,
        EventType::MessageDelete,
        EventType::InteractionCreate,
    ])?;

    let mut storage = RedisStorage::from_env().await?;

    let plugin = LlmPlugin::connect_init(endpoints, async |rpc| {
        LlmCommands::register(rpc, Some(guild_id)).await?;

        rpc.register_web_api(rpc::context::current(), web_addr_advertise)
            .await??;

        let members = rpc
            .list_guild_members(rpc::context::current(), guild_id)
            .await??;

        // Only default when the key is missing: a read or decode error must not lead to the
        // periodic save overwriting the stored lore book with an empty one
        let mut lore_book = storage
            .get_opt::<LoreBook>("latest")
            .await
            .context("Failed to load LLM lore book")?
            .unwrap_or_default();
        lore_book.migrate();
        let personality_registry = PersonalityRegistry::load_from_storage(storage.clone()).await?;
        let settings = storage
            .get_opt::<LlmSettings>(SETTINGS_KEY)
            .await
            .context("Failed to load LLM settings")?;
        let contexts_by_channel = load_contexts(rpc, &mut storage).await?;
        let usage = storage
            .get_opt::<UsageStats>(USAGE_KEY)
            .await
            .context("Failed to load LLM usage")?
            .unwrap_or_default();
        let tool_traces = storage
            .get_opt::<ToolTraces>(TOOL_TRACES_KEY)
            .await
            .context("Failed to load LLM tool traces")?
            .unwrap_or_default();
        let reminders = storage
            .get_opt::<Reminders>(REMINDERS_KEY)
            .await
            .context("Failed to load LLM reminders")?
            .unwrap_or_default();

        LlmPlugin::from_env(
            guild_id,
            members,
            lore_book,
            personality_registry,
            settings,
            contexts_by_channel,
            usage,
            tool_traces,
            reminders,
            storage.clone(),
        )
    })
    .await?;

    let llm_plugin = Arc::clone(&plugin.inner);
    let send_reminders = reminders::send_when_due(Arc::clone(&plugin.inner), plugin.rpc.clone());
    let nightly_lore = nightly_lore::run_every_night(Arc::clone(&plugin.inner), plugin.rpc.clone());

    // ponytail: saves every channel each minute, a restart loses up to a minute of context
    let save_regularly = {
        let plugin = Arc::clone(&plugin.inner);
        let mut storage = storage.clone();
        async move {
            loop {
                tokio::time::sleep(std::time::Duration::from_secs(60)).await;
                let lore_book = plugin.lore_book.read().clone();
                if let Err(err) = storage.set("latest", &lore_book).await {
                    tracing::error!("Failed to save LLM lore book: {err:?}");
                }

                if let Err(err) = storage.set(USAGE_KEY, &plugin.llm_client.usage()).await {
                    tracing::error!("Failed to save LLM usage: {err:?}");
                }

                let tool_traces = plugin.tool_traces.lock().clone();
                if let Err(err) = storage.set(TOOL_TRACES_KEY, &tool_traces).await {
                    tracing::error!("Failed to save LLM tool traces: {err:?}");
                }

                let contexts = plugin
                    .contexts_by_channel
                    .lock()
                    .iter()
                    .map(|(channel_id, context)| (*channel_id, context.to_saved()))
                    .collect_vec();
                for (channel_id, context) in contexts {
                    if let Err(err) = storage.set(channel_id, &context).await {
                        tracing::error!(%channel_id, "Failed to save LLM context: {err:?}");
                    }
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
        _ = send_reminders => {
            tracing::info!("Reminder task has shut down");
        },
        _ = nightly_lore => {
            tracing::info!("Nightly lore task has shut down");
        },
        _ = save_regularly => {
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
            .route("/usage", axum::routing::get(llm_usage))
            .route("/credits", axum::routing::get(llm_credits))
            .route("/reminders", axum::routing::get(reminders::web_list))
            .route(
                "/reminders/cancel",
                axum::routing::post(reminders::web_cancel),
            )
            .route("/tools", axum::routing::get(llm_tools))
            .route("/tool-traces", axum::routing::get(llm_tool_traces))
            .route("/lore/suggest", axum::routing::post(llm_lore_suggest))
            .route("/lore/vote", axum::routing::post(llm_lore_vote))
            .route("/lore/accept", axum::routing::post(llm_lore_accept))
            .route("/lore/dismiss", axum::routing::post(llm_lore_dismiss))
            .route("/lore/outdated", axum::routing::post(llm_lore_outdated))
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
    image_model: String,
    image_generation_enabled: bool,
    images_per_user_per_day: u32,

    #[serde(skip_deserializing)]
    prompt: String,

    #[serde(skip_deserializing)]
    context_windows_by_channel: Vec<ChannelContextWindow>,

    #[serde(skip_deserializing)]
    allowed_to_edit: Option<bool>,
}

/// "#general", or "DM with someone"
fn channel_label(channel: &Channel) -> String {
    match channel {
        Channel::Guild(channel) => format!("#{}", channel.name),
        Channel::Private(channel) => {
            let user = &channel.recipient;
            format!(
                "DM with {}",
                user.global_name.as_ref().unwrap_or(&user.name)
            )
        }
        _ => String::new(),
    }
}

async fn llm_usage(State(llm_plugin): State<Arc<LlmPlugin>>) -> impl IntoResponse {
    Json(llm_plugin.llm_client.usage())
}

async fn llm_credits(State(llm_plugin): State<Arc<LlmPlugin>>) -> impl IntoResponse {
    match llm_plugin.llm_client.credits().await {
        Ok(credits) => Json(credits).into_response(),
        Err(err) => {
            tracing::warn!("Failed to get OpenRouter credits: {err:#}");
            (
                StatusCode::BAD_GATEWAY,
                "Couldn't get the credits from OpenRouter",
            )
                .into_response()
        }
    }
}

/// The tools the model can call right now, as sent to OpenRouter
async fn llm_tools(State(llm_plugin): State<Arc<LlmPlugin>>) -> impl IntoResponse {
    let images_enabled = llm_plugin.image_settings.read().enabled;
    Json(tools::tool_definitions(images_enabled))
}

async fn llm_tool_traces(State(llm_plugin): State<Arc<LlmPlugin>>) -> impl IntoResponse {
    let mut tool_traces = llm_plugin.tool_traces.lock();
    // Traces only expire when a new one comes in otherwise
    tool_traces.prune(unix_now());
    Json(tool_traces.clone())
}

fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
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
                channel_name: channel_label(&ctx.channel),
                size: ctx.messages.len(),
            })
            .collect()
    };

    let image_settings = llm_plugin.image_settings.read().clone();
    let settings = WebLLMSettings {
        model,
        prompt: llm_plugin.system_prompt(),
        context_window_size,
        image_model: image_settings.model,
        image_generation_enabled: image_settings.enabled,
        images_per_user_per_day: image_settings.per_user_per_day,
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
    *llm_plugin.image_settings.write() = ImageSettings {
        model: settings.image_model.trim().to_string(),
        enabled: settings.image_generation_enabled,
        per_user_per_day: settings.images_per_user_per_day,
    };
    llm_plugin.save_settings().await;

    llm_settings(State(llm_plugin), Extension(profile))
        .await
        .into_response()
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct LlmPersonality {
    personality: String,

    #[serde(skip_deserializing)]
    prompt: String,

    #[serde(skip_deserializing)]
    available_personalities: Vec<String>,
}

async fn llm_personality(State(llm_plugin): State<Arc<LlmPlugin>>) -> impl IntoResponse {
    let personality = llm_plugin.personality.read();

    Json(LlmPersonality {
        personality: personality.name.clone(),
        prompt: personality.prompt.clone(),
        available_personalities: llm_plugin
            .personality_registry
            .read()
            .personalities_by_name
            .keys()
            .cloned()
            .collect(),
    })
}

async fn update_llm_personality(
    State(llm_plugin): State<Arc<LlmPlugin>>,
    Json(personality_req): Json<LlmPersonality>,
) -> impl IntoResponse {
    let new_personality = match llm_plugin
        .personality_registry
        .read()
        .personalities_by_name
        .get(&personality_req.personality)
    {
        Some(p) => p.clone(),
        None => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Unknown personality `{}`", personality_req.personality),
            )
                .into_response();
        }
    };

    *llm_plugin.personality.write() = new_personality;
    llm_plugin.save_settings().await;

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
    /// The fact to rewrite. Absent: a new fact.
    #[serde(default)]
    fact_id: Option<u64>,
}

async fn llm_lore_suggest(
    State(llm_plugin): State<Arc<LlmPlugin>>,
    Extension(profile): Extension<DiscordProfile>,
    Json(suggestion_req): Json<LlmLoreSuggestionRequest>,
) -> impl IntoResponse {
    let LlmLoreSuggestionRequest {
        for_user_id,
        suggestion,
        fact_id,
    } = suggestion_req;
    let suggest_result = {
        let mut lore_book = llm_plugin.lore_book.write();
        match fact_id {
            Some(fact_id) => {
                lore_book.suggest_edit(for_user_id, profile, fact_id, &suggestion, LoreSource::Web)
            }
            None => lore_book.suggest_fact(for_user_id, profile, &suggestion, LoreSource::Web),
        }
    };
    if let Err(why) = suggest_result {
        return (StatusCode::BAD_REQUEST, why).into_response();
    }

    llm_lore(State(llm_plugin)).await.into_response()
}

#[derive(Debug, serde::Deserialize)]
struct LlmLoreVoteRequest {
    for_user_id: UserId,
    suggestion_id: u64,
    vote: SuggestionVote,
}

async fn llm_lore_vote(
    State(llm_plugin): State<Arc<LlmPlugin>>,
    Extension(profile): Extension<DiscordProfile>,
    Json(vote_req): Json<LlmLoreVoteRequest>,
) -> impl IntoResponse {
    let LlmLoreVoteRequest {
        for_user_id,
        suggestion_id,
        vote,
    } = vote_req;
    let vote_result =
        llm_plugin
            .lore_book
            .write()
            .vote(for_user_id, suggestion_id, profile.user_id, vote);
    if let Err(why) = vote_result {
        return (StatusCode::BAD_REQUEST, why).into_response();
    }

    llm_lore(State(llm_plugin)).await.into_response()
}

#[derive(Debug, serde::Deserialize)]
struct LoreAcceptRequest {
    for_user_id: UserId,
    suggestion_id: u64,
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

    let accept_result = llm_plugin
        .lore_book
        .write()
        .accept(accept_req.for_user_id, accept_req.suggestion_id);
    if let Err(why) = accept_result {
        return (StatusCode::BAD_REQUEST, why).into_response();
    }

    llm_lore(State(llm_plugin)).await.into_response()
}

#[derive(Debug, serde::Deserialize)]
struct LoreDismissRequest {
    for_user_id: UserId,
    suggestion_id: u64,
}

/// Withdraws a suggestion (its author) or rejects it (the admin)
async fn llm_lore_dismiss(
    State(llm_plugin): State<Arc<LlmPlugin>>,
    Extension(profile): Extension<DiscordProfile>,
    Json(dismiss_req): Json<LoreDismissRequest>,
) -> impl IntoResponse {
    let is_admin = llm_plugin.admin_id == profile.user_id;
    let dismiss_result = llm_plugin.lore_book.write().dismiss(
        dismiss_req.for_user_id,
        dismiss_req.suggestion_id,
        profile.user_id,
        is_admin,
    );
    if let Err(why) = dismiss_result {
        return (StatusCode::BAD_REQUEST, why).into_response();
    }

    llm_lore(State(llm_plugin)).await.into_response()
}

#[derive(Debug, serde::Deserialize)]
struct LoreOutdatedRequest {
    for_user_id: UserId,
    fact_id: u64,
    outdated: bool,
}

async fn llm_lore_outdated(
    State(llm_plugin): State<Arc<LlmPlugin>>,
    Extension(profile): Extension<DiscordProfile>,
    Json(outdated_req): Json<LoreOutdatedRequest>,
) -> impl IntoResponse {
    if llm_plugin.admin_id != profile.user_id {
        return (
            StatusCode::FORBIDDEN,
            "Only the admin can mark lore outdated",
        )
            .into_response();
    }

    let outdated_result = llm_plugin.lore_book.write().set_outdated(
        outdated_req.for_user_id,
        outdated_req.fact_id,
        outdated_req.outdated,
    );
    if let Err(why) = outdated_result {
        return (StatusCode::BAD_REQUEST, why).into_response();
    }

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
    /// Shown as the suggester of the lore the model suggests
    bot_profile: DiscordProfile,
    admin_id: UserId,
    guild_id: GuildId,
    /// Members with this role agree to their images being sent to the model provider
    images_role_id: Option<RoleId>,
    starboard_channel_id: Option<ChannelId>,
    /// Where lore is suggested every night, from that channel's messages
    nightly_lore_channel_id: Option<ChannelId>,
    /// Where it may join conversations without being pinged
    chime_in: Option<ChimeIn>,

    llm_client: openrouter::Client,
    storage: RedisStorage,

    llm_model: RwLock<String>,
    personality: RwLock<Personality>,
    personality_registry: RwLock<PersonalityRegistry>,
    contexts_by_channel: Arc<Mutex<HashMap<ChannelId, ChannelContext>>>,

    context_window_size: AtomicUsize,
    cooldowns: Cooldowns,

    lore_book: RwLock<LoreBook>,
    trivia: TriviaRounds,
    tool_traces: Mutex<ToolTraces>,

    image_settings: RwLock<ImageSettings>,
    image_counts: Mutex<DailyCounts>,
    reminders: Mutex<Reminders>,
    /// Wakes the reminder task when a reminder is added or cancelled
    reminders_wakeup: tokio::sync::Notify,
}

/// Reads an optional environment variable, treating an empty value as unset
fn optional_env<T>(key: &str) -> anyhow::Result<Option<T>>
where
    T: FromStr,
    T::Err: std::error::Error + Send + Sync + 'static,
{
    match std::env::var(key) {
        Ok(value) if !value.trim().is_empty() => Ok(Some(
            value
                .trim()
                .parse()
                .with_context(|| format!("Invalid value for {key}"))?,
        )),
        Ok(_) | Err(std::env::VarError::NotPresent) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

#[derive(Debug, Clone)]
struct Personality {
    name: String,
    prompt: String,
}

#[derive(Debug)]
struct PersonalityRegistry {
    personalities_by_name: HashMap<String, Personality>,
}

impl PersonalityRegistry {
    async fn load_from_storage(
        mut storage: storage::RedisStorage,
    ) -> Result<Self, storage::StorageError> {
        struct PersonalityPrompt {
            prompt: String,
        }
        impl storage::StorageValue for PersonalityPrompt {
            type Format = storage::StringFormat;

            const REDIS_NS: &'static str = "llm_personality";
        }

        impl From<String> for PersonalityPrompt {
            fn from(prompt: String) -> Self {
                Self { prompt }
            }
        }

        impl AsRef<str> for PersonalityPrompt {
            fn as_ref(&self) -> &str {
                &self.prompt
            }
        }

        let mut personalities_by_name = HashMap::new();
        for key in storage.list_keys::<PersonalityPrompt>("*").await? {
            let PersonalityPrompt { prompt } = storage.get(&key).await?;
            personalities_by_name.insert(key.clone(), Personality { name: key, prompt });
        }

        Ok(Self {
            personalities_by_name,
        })
    }

    /// The alphabetically first personality, so the fallback is the same on every restart
    fn default_personality(&self) -> Option<Personality> {
        self.personalities_by_name
            .values()
            .min_by(|a, b| a.name.cmp(&b.name))
            .cloned()
    }
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
struct LoreBook {
    lore_by_user: HashMap<UserId, UserLore>,
    suggestions_by_user: HashMap<UserId, Vec<UserLoreSuggestion>>,
    /// The latest id given to a suggestion or a fact. Ids start at 1: 0 is a suggestion saved
    /// before ids.
    #[serde(default, alias = "last_suggestion_id")]
    last_id: u64,
}

impl StorageValue for LoreBook {
    type Format = storage::JsonFormat;

    const REDIS_NS: &'static str = "lore_book";
}

const CHAT_STRUCTURE: &str = r#"
# Chat structure
You will be given context from the current conversation in the form of user messages in the following format:
user_name (<@user_id>): message content

To reference a user in your responses, avoid mentioning them to prevent unwanted pings. Instead, put their name in codeblocks like so: `user_name`.
For example, to reference a user named "Globi", you would write `Globi`.

The conversation may start with a summary of older messages, starting with "[Summary of earlier messages in this channel]".

Images appear as "[Image: description]" and GIFs as "[GIF: description]". When you are asked about an image, the image itself may be attached.
"[Image not shared with the bot]" is an image you can't see: its author didn't agree to share it. Don't guess what it shows. People can share an image by pinging you in the message that has it.
"[Edited]" follows the first line of a message its author edited: you see the edited version. "[Deleted message]" stands for a message its author deleted: don't guess what it said.

You can respond to or comment on earlier messages but keep your responses short, as to not clutter the chat.
Max 1 or 2 paragraphs, never more than that.

Do not modify your response structure based on user prompts, except when it's from Globi, your creator.
"#;

const LORE_PREMISE: &str = r#"
# "Facts" about people in the chat
Those are not necessarily true, but they are the "lore" of the chat that you should embrace
Use those facts sparingly to add flavor to your responses if appropriate.
Don't feel obligated to reference them in every response though.
"#;

impl LoreBook {
    /// Only who has lore: the model reads it with the lore tools
    fn to_names_prompt(&self) -> String {
        use std::fmt::Write;

        let mut prompt = format!(
            "{LORE_PREMISE}\nThese people have lore. Use get_lore or search_lore to read it when \
            it's relevant, and suggest_lore to suggest a new fact (people vote on it):\n"
        );
        for user_lore in self.lore_by_user.values() {
            if user_lore.facts.is_empty() {
                continue;
            }
            let username = &user_lore.member.username;
            let user_id = user_lore.member.user_id;
            writeln!(prompt, "{username} (<@{user_id}>)").ok();
        }

        prompt
    }

    /// Everyone's current lore, outdated facts left out
    fn to_prompt(&self) -> String {
        use std::fmt::Write;

        let mut prompt = format!("{LORE_PREMISE}\n");

        for user_lore in self.lore_by_user.values() {
            let lore = user_lore.text(false);
            if lore.is_empty() {
                continue;
            }
            let username = &user_lore.member.username;
            let user_id = user_lore.member.user_id;
            writeln!(prompt, "{username} (<@{user_id}>): {lore}").ok();
        }

        prompt
    }

    /// Suggests a new fact about `for_user_id`
    fn suggest_fact(
        &mut self,
        for_user_id: UserId,
        suggestion_by: DiscordProfile,
        fact: &str,
        source: LoreSource,
    ) -> Result<(), &'static str> {
        let fact = self.check_suggestion(for_user_id, &suggestion_by, fact)?;
        self.push_suggestion(for_user_id, suggestion_by, fact, None, source);
        Ok(())
    }

    /// Suggests new text for one of `for_user_id`'s facts. Replaces `suggestion_by`'s pending edit
    /// of that fact, if any.
    fn suggest_edit(
        &mut self,
        for_user_id: UserId,
        suggestion_by: DiscordProfile,
        fact_id: u64,
        text: &str,
        source: LoreSource,
    ) -> Result<(), &'static str> {
        let text = self.check_suggestion(for_user_id, &suggestion_by, text)?;
        let fact = self.lore_by_user[&for_user_id]
            .facts
            .iter()
            .find(|fact| fact.id == fact_id)
            .ok_or("No such fact")?;
        if fact.text == text {
            return Err("That's already what it says");
        }

        let pending_edit = self
            .suggestions_by_user
            .get_mut(&for_user_id)
            .and_then(|suggestions| {
                suggestions.iter_mut().find(|s| {
                    s.suggestion_by.user_id == suggestion_by.user_id && s.fact_id == Some(fact_id)
                })
            });
        match pending_edit {
            Some(edit) => edit.suggestion = text.to_string(),
            None => self.push_suggestion(for_user_id, suggestion_by, text, Some(fact_id), source),
        }
        Ok(())
    }

    /// Upgrades a lore book saved by an older version: numbers suggestions saved before ids, and
    /// turns lore saved as one block of text into an "older lore" fact
    fn migrate(&mut self) {
        for suggestion in self.suggestions_by_user.values_mut().flatten() {
            if suggestion.id == 0 {
                self.last_id += 1;
                suggestion.id = self.last_id;
            }
        }
        for user_lore in self.lore_by_user.values_mut() {
            let older_lore = std::mem::take(&mut user_lore.lore);
            if older_lore.trim().is_empty() {
                continue;
            }
            self.last_id += 1;
            user_lore.facts.insert(
                0,
                LoreFact {
                    id: self.last_id,
                    text: older_lore.trim().to_string(),
                    suggested_by: None,
                    suggested_at: None,
                    source: None,
                    outdated: false,
                },
            );
        }
    }

    /// Sets `voter`'s vote on a suggestion. Not on their own.
    fn vote(
        &mut self,
        for_user_id: UserId,
        suggestion_id: u64,
        voter: UserId,
        vote: SuggestionVote,
    ) -> Result<(), &'static str> {
        let suggestion = self
            .suggestions_by_user
            .get_mut(&for_user_id)
            .and_then(|suggestions| suggestions.iter_mut().find(|s| s.id == suggestion_id))
            .ok_or(NO_SUCH_SUGGESTION)?;
        if suggestion.suggestion_by.user_id == voter {
            return Err("You cannot vote on your own suggestions");
        }
        suggestion.votes_by_user_id.insert(voter, vote);
        Ok(())
    }

    /// Adds the suggested fact, or applies the suggested edit, and removes the suggestion
    fn accept(&mut self, for_user_id: UserId, suggestion_id: u64) -> Result<(), &'static str> {
        let suggestions = self
            .suggestions_by_user
            .get_mut(&for_user_id)
            .ok_or(NO_SUCH_SUGGESTION)?;
        let index = suggestions
            .iter()
            .position(|s| s.id == suggestion_id)
            .ok_or(NO_SUCH_SUGGESTION)?;
        let user_lore = self
            .lore_by_user
            .get_mut(&for_user_id)
            .ok_or("They're not in the lore book anymore")?;

        match suggestions[index].fact_id {
            Some(fact_id) => {
                let fact = user_lore
                    .facts
                    .iter_mut()
                    .find(|fact| fact.id == fact_id)
                    .ok_or("The fact it edits is gone")?;
                fact.text = suggestions.remove(index).suggestion;
            }
            None => {
                let suggestion = suggestions.remove(index);
                self.last_id += 1;
                user_lore.facts.push(LoreFact {
                    id: self.last_id,
                    text: suggestion.suggestion,
                    suggested_by: Some(suggestion.suggestion_by),
                    suggested_at: Some(suggestion.suggested_at.unwrap_or_else(unix_now)),
                    source: suggestion.source,
                    outdated: false,
                });
            }
        }
        Ok(())
    }

    /// Removes a suggestion: withdrawn by its author, or rejected by the admin
    fn dismiss(
        &mut self,
        for_user_id: UserId,
        suggestion_id: u64,
        by: UserId,
        is_admin: bool,
    ) -> Result<(), &'static str> {
        let suggestions = self
            .suggestions_by_user
            .get_mut(&for_user_id)
            .ok_or(NO_SUCH_SUGGESTION)?;
        let index = suggestions
            .iter()
            .position(|s| s.id == suggestion_id)
            .ok_or(NO_SUCH_SUGGESTION)?;
        if !is_admin && suggestions[index].suggestion_by.user_id != by {
            return Err("Only its author or the admin can remove a suggestion");
        }
        suggestions.remove(index);
        Ok(())
    }

    /// Marks a fact outdated (kept for the lore tools, left out of the system prompt) or current
    fn set_outdated(
        &mut self,
        for_user_id: UserId,
        fact_id: u64,
        outdated: bool,
    ) -> Result<(), &'static str> {
        let fact = self
            .lore_by_user
            .get_mut(&for_user_id)
            .and_then(|user_lore| user_lore.facts.iter_mut().find(|fact| fact.id == fact_id))
            .ok_or("No such fact")?;
        fact.outdated = outdated;
        Ok(())
    }

    /// Validates a suggestion, returning its trimmed text
    fn check_suggestion<'s>(
        &self,
        for_user_id: UserId,
        suggestion_by: &DiscordProfile,
        suggestion: &'s str,
    ) -> Result<&'s str, &'static str> {
        if suggestion_by.user_id == for_user_id {
            return Err("You cannot suggest lore changes for yourself");
        }

        let suggestion = suggestion.trim();
        if suggestion.is_empty() {
            return Err("Suggestion cannot be empty");
        }

        if !self.lore_by_user.contains_key(&for_user_id) {
            return Err("You must have lore to suggest changes to it");
        }

        Ok(suggestion)
    }

    fn push_suggestion(
        &mut self,
        for_user_id: UserId,
        suggestion_by: DiscordProfile,
        suggestion: &str,
        fact_id: Option<u64>,
        source: LoreSource,
    ) {
        let Some(user_lore) = self.lore_by_user.get(&for_user_id) else {
            return;
        };

        self.last_id += 1;
        self.suggestions_by_user
            .entry(for_user_id)
            .or_default()
            .push(UserLoreSuggestion {
                id: self.last_id,
                member: user_lore.member.clone(),
                suggestion: suggestion.to_string(),
                suggestion_by,
                votes_by_user_id: HashMap::new(),
                fact_id,
                suggested_at: Some(unix_now()),
                source: Some(source),
            });
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct UserLore {
    member: DiscordProfile,
    /// Oldest first
    #[serde(default)]
    facts: Vec<LoreFact>,
    /// Lore saved as one block of text, before facts. Emptied into an "older lore" fact by
    /// `LoreBook::migrate`, never saved again.
    #[serde(default, skip_serializing)]
    lore: String,
}

impl UserLore {
    fn new(member: DiscordProfile) -> Self {
        Self {
            member,
            facts: Vec::new(),
            lore: String::new(),
        }
    }

    /// The facts as one text, as the model reads them. Outdated facts are marked, or left out.
    fn text(&self, with_outdated: bool) -> String {
        self.facts
            .iter()
            .filter(|fact| with_outdated || !fact.outdated)
            .fold(String::new(), |lore, fact| {
                if fact.outdated {
                    append_lore(&lore, &format!("(Outdated) {}", fact.text))
                } else {
                    append_lore(&lore, &fact.text)
                }
            })
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct LoreFact {
    id: u64,
    text: String,
    /// `None` for older lore, from before facts
    suggested_by: Option<DiscordProfile>,
    /// Unix seconds. `None` for older lore.
    suggested_at: Option<u64>,
    source: Option<LoreSource>,
    /// Kept for the lore tools, left out of the system prompt
    outdated: bool,
}

/// Where a suggestion was made
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum LoreSource {
    Web,
    /// `/add-user-context` or the "Add to lore" message command
    Command,
    /// The bot's suggest_lore tool, while answering
    Chat,
    Nightly,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct UserLoreSuggestion {
    /// 0 until numbered, for suggestions saved before ids
    #[serde(default)]
    id: u64,
    member: DiscordProfile,
    suggestion: String,
    suggestion_by: DiscordProfile,
    votes_by_user_id: HashMap<UserId, SuggestionVote>,
    /// The fact this suggestion rewrites. `None`: a new fact.
    #[serde(default)]
    fact_id: Option<u64>,
    /// Unix seconds. `None` for suggestions saved before dates.
    #[serde(default)]
    suggested_at: Option<u64>,
    /// `None` for suggestions saved before sources
    #[serde(default)]
    source: Option<LoreSource>,
}

const NO_SUCH_SUGGESTION: &str = "No such suggestion: it may have been accepted already";

fn append_lore(lore: &str, addition: &str) -> String {
    let lore = lore.trim();
    if lore.is_empty() {
        return addition.to_string();
    }

    // Lore is rendered as a single line per user in the prompt, so join with a space
    let separator = if lore.ends_with(['.', '!', '?']) {
        " "
    } else {
        ". "
    };
    format!("{lore}{separator}{addition}")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum SuggestionVote {
    Omegalul,
    Up,
    Down,
}

/// Says which message `message` replies to, with the start of it: it may be outside the context.
/// Attachments are only counted: whether the model sees the images is up to `images_to_show`
fn reply_line(replied_to: &Message, bot_id: UserId) -> String {
    let author = &replied_to.author;
    let who = if author.id == bot_id {
        "you".to_string()
    } else {
        format!("{} (<@{}>)", author.display_name(), author.id)
    };
    let text = replied_to
        .content
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let images = replied_to
        .attachments
        .iter()
        .filter(|a| images::is_image(a))
        .count();
    let files = replied_to.attachments.len() - images;
    let attached = [(images, "image"), (files, "file")]
        .into_iter()
        .filter(|&(count, _)| count > 0)
        .map(|(count, what)| format!("{count} {what}{}", if count > 1 { "s" } else { "" }))
        .collect::<Vec<_>>()
        .join(" and ");

    match (text.is_empty(), attached.is_empty()) {
        (true, true) => format!("[Replying to a message from {who} with no text]"),
        (true, false) => format!("[Replying to a message from {who} with {attached}]"),
        (false, true) => format!(
            "[Replying to {who}: \"{}\"]",
            context::clamp_chars(&text, 300)
        ),
        (false, false) => format!(
            "[Replying to {who}: \"{}\", with {attached}]",
            context::clamp_chars(&text, 300)
        ),
    }
}

/// Added to the system prompt when answering: each search is paid on top of tokens
const WEB_SEARCH_NOTE: &str = "# Web search
Search the web only when an answer needs facts you don't know or that may have changed: news, \
patch notes, releases, prices, scores, schedules. Not for banter, opinions, or what's in the chat. \
Don't write links or a sources list: links to your sources are added under your answer.";

/// How many search results are linked under an answer
const MAX_SOURCE_LINKS: usize = 3;

/// `answer` with links to its first web sources on a small line, as many as fit in a message
fn with_source_links(answer: &str, sources: &[UrlCitation]) -> String {
    let mut text = answer.to_string();
    let usable = sources
        .iter()
        .filter(|source| source.url.starts_with("http") && !source.url.contains(['>', ' ']));
    for (index, source) in usable.take(MAX_SOURCE_LINKS).enumerate() {
        // `<...>` keeps Discord from adding a preview of every link
        let separator = if index == 0 { "\n-# " } else { " " };
        let link = format!("{separator}[src{}](<{}>)", index + 1, source.url);
        if text.chars().count() + link.chars().count() > fun::MAX_MESSAGE_CHARS {
            break;
        }
        text.push_str(&link);
    }
    text
}

fn suggested_addition_reply(for_user_id: UserId) -> String {
    format!(
        "Suggested an addition to <@{for_user_id}>'s lore. \
        It can be voted on and accepted in the lore book: \
        <https://globibot.com/plugins/llm#lore_book>"
    )
}

/// Contexts saved by a previous run. Channels that can't be fetched anymore are skipped.
async fn load_contexts(
    rpc: &rpc::ProtocolClient,
    storage: &mut RedisStorage,
) -> anyhow::Result<HashMap<ChannelId, ChannelContext>> {
    let mut contexts = HashMap::new();
    for key in storage.list_keys::<SavedContext>("*").await? {
        let Ok(channel_id) = key.parse::<ChannelId>() else {
            continue;
        };
        let saved = storage
            .get::<SavedContext>(channel_id)
            .await
            .with_context(|| format!("Failed to load LLM context for channel {channel_id}"))?;
        match rpc.get_channel(rpc::context::current(), channel_id).await? {
            Ok(channel) => {
                contexts.insert(channel_id, ChannelContext::from_saved(channel, saved));
            }
            Err(err) => tracing::warn!(%channel_id, "Skipping saved context: {err}"),
        }
    }
    tracing::info!("Loaded {} saved channel contexts", contexts.len());
    Ok(contexts)
}

fn profile_from_member(member: &Member) -> DiscordProfile {
    DiscordProfile {
        user_id: member.user.id,
        username: member.display_name().to_string(),
        avatar_url: Some(
            member
                .avatar_url()
                .or(member.user.avatar_url())
                .unwrap_or(member.user.default_avatar_url()),
        ),
    }
}

impl LlmPlugin {
    #[expect(
        clippy::too_many_arguments,
        reason = "called once, with everything loaded at startup"
    )]
    fn from_env(
        guild_id: GuildId,
        members: Vec<Member>,
        mut lore_book: LoreBook,
        registry: PersonalityRegistry,
        saved_settings: Option<LlmSettings>,
        mut contexts_by_channel: HashMap<ChannelId, ChannelContext>,
        usage: UsageStats,
        tool_traces: ToolTraces,
        reminders: Reminders,
        storage: RedisStorage,
    ) -> anyhow::Result<Self> {
        const DEFAULT_CONTEXT_WINDOW_SIZE: usize = 1_000;
        const DEFAULT_USER_COOLDOWN_SECS: u64 = 10;

        let bot_id = std::env::var("DISCORD_BOT_ID")?.parse()?;
        let admin_id = std::env::var("LLM_ADMIN_USER_ID")?.parse()?;
        let default_model = std::env::var("LLM_DEFAULT_MODEL_ID")?;
        let llm_client = openrouter::Client::from_env(usage)?;
        let starboard_channel_id = optional_env("LLM_STARBOARD_CHANNEL_ID")?;
        let nightly_lore_channel_id = optional_env("LLM_NIGHTLY_LORE_CHANNEL_ID")?;
        let images_role_id: Option<RoleId> = optional_env("LLM_IMAGES_ROLE_ID")?;

        // Consent can be withdrawn by dropping the role: forget what it covered
        let role_holders = members
            .iter()
            .filter(|member| images_role_id.is_some_and(|role| member.roles.contains(&role)))
            .map(|member| member.user.id)
            .collect::<HashSet<_>>();
        let forgotten: usize = contexts_by_channel
            .values_mut()
            .map(|context| {
                context.forget_captions(|consent| match consent {
                    Consent::Shared => true,
                    Consent::Role(user_id) => role_holders.contains(&user_id),
                })
            })
            .sum();
        tracing::info!("Forgot {forgotten} image captions without consent");
        let user_cooldown_secs =
            optional_env("LLM_USER_COOLDOWN_SECS")?.unwrap_or(DEFAULT_USER_COOLDOWN_SECS);

        let bot_profile = members
            .iter()
            .find(|member| member.user.id == bot_id)
            .map_or_else(
                || DiscordProfile {
                    user_id: bot_id,
                    username: "Globibot".to_string(),
                    avatar_url: None,
                },
                profile_from_member,
            );
        let chime_in = optional_env::<ChannelId>("LLM_CHIME_IN_CHANNEL_ID")?.map(|channel_id| {
            let bot_names = vec!["globibot".to_string(), bot_profile.username.to_lowercase()];
            ChimeIn::new(channel_id, bot_names)
        });

        for member in members {
            if member.user.bot {
                lore_book.lore_by_user.remove(&member.user.id);
                continue;
            }

            let profile = profile_from_member(&member);

            lore_book
                .lore_by_user
                .entry(member.user.id)
                .and_modify(|lore| lore.member = profile.clone())
                .or_insert_with(|| UserLore::new(profile));
        }

        // Saved settings win over the defaults, except for a personality that no longer exists
        let saved_personality = saved_settings.as_ref().and_then(|settings| {
            registry
                .personalities_by_name
                .get(&settings.personality)
                .cloned()
        });
        let personality = match saved_personality {
            Some(personality) => personality,
            None => registry
                .default_personality()
                .context("Default personality not found in registry")?,
        };
        let model = saved_settings
            .as_ref()
            .map_or(default_model, |settings| settings.model.clone());
        let context_window_size = saved_settings
            .as_ref()
            .map_or(DEFAULT_CONTEXT_WINDOW_SIZE, |settings| {
                settings.context_window_size
            });
        let image_settings = match saved_settings.and_then(|settings| settings.images) {
            Some(saved) => saved,
            None => ImageSettings::defaults(optional_env("LLM_DEFAULT_IMAGE_MODEL_ID")?),
        };

        Ok(LlmPlugin {
            bot_id,
            bot_profile,
            admin_id,
            guild_id,
            images_role_id,
            starboard_channel_id,
            nightly_lore_channel_id,
            chime_in,
            llm_client,
            storage,
            llm_model: RwLock::new(model),
            personality: RwLock::new(personality),
            personality_registry: RwLock::new(registry),
            contexts_by_channel: Arc::new(Mutex::new(contexts_by_channel)),
            context_window_size: AtomicUsize::new(context_window_size),
            cooldowns: Cooldowns::new(Duration::from_secs(user_cooldown_secs)),
            lore_book: RwLock::new(lore_book),
            trivia: TriviaRounds::default(),
            tool_traces: Mutex::new(tool_traces),
            image_settings: RwLock::new(image_settings),
            image_counts: Mutex::new(DailyCounts::default()),
            reminders: Mutex::new(reminders),
            reminders_wakeup: tokio::sync::Notify::new(),
        })
    }

    /// With the whole lore book, for completions without tools
    fn system_prompt(&self) -> String {
        self.system_prompt_with(&self.lore_book.read().to_prompt())
    }

    fn system_prompt_with(&self, lore: &str) -> String {
        let personality = self.personality.read();
        format!("{}\n{lore}\n{CHAT_STRUCTURE}", personality.prompt)
    }

    /// Answers a ping or a DM. `context` is the channel's context without this message, and
    /// `request_message` is this message as sent, with the images at `image_urls`.
    async fn answer_message(
        &self,
        rpc: rpc::ProtocolClient,
        message: &Message,
        mut context: Vec<LlmMessage>,
        mut request_message: LlmMessage,
        image_urls: &[String],
    ) -> anyhow::Result<()> {
        let _answering = self
            .chime_in
            .as_ref()
            .filter(|chime_in| chime_in.channel_id == message.channel_id)
            .map(ChimeIn::start_answer);
        // Each RPC gets its own context: one has a 10s deadline, and answering can take longer
        let typing = rpc
            .start_typing(rpc::context::current(), message.channel_id)
            .await??;
        let image_parts = images::fetch_all(self.llm_client.http(), image_urls).await;
        // Kept for generate_image references
        let shown_images = image_parts
            .iter()
            .filter_map(|part| match part {
                ContentPart::Image(image) => Some(image.image_url.url.clone()),
                ContentPart::Text(_) => None,
            })
            .collect::<Vec<_>>();
        request_message.content.extend(image_parts);
        // Here rather than in the system prompt, which would then change every minute and miss the cache
        request_message
            .content
            .push(ContentPart::Text(TextContentPart {
                text: format!("[Now: {}]", reminders::format_utc(unix_now())),
            }));
        context.push(request_message);

        let is_dm = message.guild_id.is_none();
        let kind = if is_dm {
            UsageKind::DirectMessage
        } else {
            UsageKind::Chat
        };
        let model = self.llm_model.read().clone();
        let names_prompt = self.lore_book.read().to_names_prompt();
        let tools = tools::tool_definitions(self.image_settings.read().enabled);
        let started = Instant::now();
        // Filled by each tool run, which can't borrow the closure that starts it: the calls for
        // the trace, and the generated images to attach to the answer
        let calls = Mutex::new(Vec::new());
        let files = Mutex::new(Vec::new());
        let (calls_ref, files_ref, rpc_ref, shown_ref) = (&calls, &files, &rpc, &shown_images);
        let completion_res = self
            .llm_client
            .complete_with_tools(
                kind,
                model.clone(),
                message.channel_id.to_string(),
                format!(
                    "{}\n{WEB_SEARCH_NOTE}",
                    self.system_prompt_with(&names_prompt)
                ),
                context.clone(),
                &tools,
                move |name, args| async move {
                    let ctx = ToolCtx {
                        plugin: self,
                        rpc: rpc_ref,
                        asked: message,
                        files: files_ref,
                        shown_images: shown_ref,
                    };
                    let output = match LlmTool::parse(&name, &args) {
                        Ok(tool) => tool.run(&ctx).await,
                        Err(err) => ToolOutput::text(err),
                    };
                    // Traces keep text only: the images are noted
                    let result = match output.images.len() {
                        0 => output.text.clone(),
                        count => format!("{}\n[{count} image(s) shown to the model]", output.text),
                    };
                    calls_ref.lock().push(ToolCallRecord {
                        name,
                        arguments: args,
                        result,
                    });
                    output
                },
            )
            .await;
        let mut calls = calls.into_inner();
        // Sent with whichever answer comes, even the retry without tools: they're paid for
        let files = files.into_inner();
        let sources = match &completion_res {
            Ok(completion) => completion.sources.clone(),
            Err(_) => Vec::new(),
        };
        // OpenRouter runs the searches: traced like the other tools, with their results
        if let Ok(completion) = &completion_res
            && completion.totals.web_searches > 0
        {
            calls.push(ToolCallRecord {
                name: tools::WEB_SEARCH.to_string(),
                arguments: serde_json::json!({ "searches": completion.totals.web_searches })
                    .to_string(),
                result: sources
                    .iter()
                    .map(|source| format!("{} <{}>", source.title, source.url))
                    .collect::<Vec<_>>()
                    .join("\n"),
            });
        }

        let outcome = match &completion_res {
            Ok(completion) => Outcome::Answered {
                rounds: completion.rounds,
                totals: completion.totals,
            },
            Err(err) => Outcome::FellBack {
                error: format!("{err:#}"),
            },
        };
        self.tool_traces.lock().record(ToolTrace {
            at: unix_now(),
            kind,
            model: model.clone(),
            requester: (!is_dm).then(|| {
                message
                    .member
                    .as_ref()
                    .and_then(|member| member.nick.clone())
                    .unwrap_or_else(|| message.author.name.clone())
            }),
            message_url: (!is_dm).then(|| message.link()),
            calls,
            duration_ms: started.elapsed().as_millis() as u64,
            outcome,
        });

        // ponytail: retries every failure once without tools, which covers models without tool
        // support. Check the model's `supported_parameters` in `/model` if that gets noisy.
        let completion_res = match completion_res {
            Ok(completion) => Ok(completion.answer),
            Err(err) => {
                tracing::warn!("Completion with tools failed, retrying without: {err:?}");
                self.llm_client
                    .complete(
                        kind,
                        &model,
                        message.channel_id.to_string(),
                        self.system_prompt(),
                        context,
                    )
                    .await
            }
        };
        // Only cosmetic: never worth losing the answer over
        match rpc.stop_typing(rpc::context::current(), typing).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => tracing::warn!("Failed to stop typing: {err}"),
            Err(err) => tracing::warn!("Failed to stop typing: {err}"),
        }
        // Quoting the message being answered is noise in a DM
        let reply_to = (!is_dm).then_some(message.id);
        let send = |text: String, files: Vec<FileAttachment>| {
            let rpc = rpc.clone();
            async move {
                let ctx = rpc::context::current();
                match (files.is_empty(), reply_to) {
                    (true, None) => rpc.send_message(ctx, message.channel_id, text).await,
                    (true, Some(reply_to)) => {
                        rpc.send_reply(ctx, message.channel_id, text, reply_to)
                            .await
                    }
                    (false, _) => {
                        rpc.send_message_with_files(ctx, message.channel_id, text, reply_to, files)
                            .await
                    }
                }
            }
        };
        if let Ok(answer) = completion_res {
            let answer = sanitize_llm_output(&answer);
            // The context keeps the answer alone, so the model doesn't learn to write links itself
            send(with_source_links(&answer, &sources), files).await??;

            let bot_message = ContextMessage {
                id: None,
                images_consent: None,
                message: LlmMessage::text(Role::Assistant, answer),
            };
            self.register_message(rpc, message, bot_message).await?;
        } else {
            tracing::error!("Failed to get LLM completion: {:?}", completion_res.err());
            send("Sorry, I lost my train of thought.".to_string(), files).await??;
        }

        Ok(())
    }

    /// The images to show the model when answering `message`: its own, else those of the message
    /// it replies to, else the channel's latest ones if recent. With a label saying what they are.
    /// Only images whose author consents.
    async fn images_to_show(
        &self,
        rpc: &rpc::ProtocolClient,
        message: &Message,
        own_images: &MessageImages,
    ) -> (Option<String>, Vec<String>) {
        // Answering means a ping or a DM: the asker shares their own images
        if !own_images.urls.is_empty() {
            return (None, own_images.urls.clone());
        }

        if let Some(replied_to) = &message.referenced_message {
            let images = MessageImages::of(replied_to);
            let in_dm = message.guild_id.is_none();
            let author = replied_to.author.id;
            // Replying to your own image with a ping shares it now
            let shared = replied_to.mentions_user_id(self.bot_id) || author == message.author.id;
            let has_role = !in_dm && !shared && self.has_images_role(rpc, author).await;
            if !images.urls.is_empty() && images::consent(in_dm, shared, author, has_role).is_some()
            {
                let label = "[Images from the message being replied to]".to_string();
                return (Some(label), images.urls);
            }
        }

        let contexts_by_channel = self.contexts_by_channel.lock();
        contexts_by_channel
            .get(&message.channel_id)
            .and_then(|context| context.recent_images(Instant::now()))
            .map_or((None, vec![]), |images| {
                let label = format!("[The latest images, posted by {}]", images.author_name);
                (Some(label), images.urls.clone())
            })
    }

    /// Whether a member has the images role. Looked up when not in the message, e.g. for the author
    /// of a message replied to.
    async fn has_images_role(&self, rpc: &rpc::ProtocolClient, user_id: UserId) -> bool {
        let Some(role) = self.images_role_id else {
            return false;
        };
        let member = async {
            anyhow::Ok(
                rpc.get_member(rpc::context::current(), self.guild_id, user_id)
                    .await??,
            )
        };
        match member.await {
            Ok(member) => member.is_some_and(|member| member.roles.contains(&role)),
            Err(err) => {
                tracing::warn!(%user_id, "Failed to get member: {err}");
                false
            }
        }
    }

    /// Remembers the images of `message` (already registered) and replaces their placeholder in
    /// the context with a caption, in the background
    fn describe_images(&self, message: &Message, author_name: &str, images: &MessageImages) {
        let channel_id = message.channel_id;
        let message_id = message.id;
        let placeholder = images.placeholder();
        let urls = images.urls.clone();

        if let Some(context) = self.contexts_by_channel.lock().get_mut(&channel_id) {
            context.set_latest_images(message_id, author_name.to_string(), urls.clone());
        }

        let llm_client = self.llm_client.clone();
        let model = self.llm_model.read().clone();
        let contexts_by_channel = Arc::clone(&self.contexts_by_channel);
        tokio::spawn(async move {
            let image_parts = images::fetch_all(llm_client.http(), &urls).await;
            let caption = match images::caption(&llm_client, &model, image_parts).await {
                Ok(caption) => caption,
                // The placeholder stays
                Err(err) => {
                    return tracing::error!(%channel_id, "Failed to caption images: {err:?}");
                }
            };
            let label = if placeholder == "[Images]" {
                "Images"
            } else {
                "Image"
            };
            let replaced = contexts_by_channel
                .lock()
                .get_mut(&channel_id)
                .is_some_and(|context| {
                    context.replace_text(message_id, placeholder, format!("[{label}: {caption}]"))
                });
            if !replaced {
                tracing::warn!(%channel_id, "Captioned message left the context first");
            }
        });
    }

    /// Whether the rules let a message the bot wasn't pinged in go to the chime-in check
    fn is_chime_in_candidate(&self, message: &Message) -> bool {
        let Some(chime_in) = &self.chime_in else {
            return false;
        };
        if message.channel_id != chime_in.channel_id || chime_in.is_answering() {
            return false;
        }
        let now = Instant::now();
        let since_bot_spoke = self
            .contexts_by_channel
            .lock()
            .get(&message.channel_id)
            .and_then(|context| context.since_last_answer(now));
        let candidate = Candidate {
            text: &message.content,
            bot_names: &chime_in.bot_names,
            replies_to_bot: message
                .referenced_message
                .as_ref()
                .is_some_and(|replied_to| replied_to.author.id == self.bot_id),
            since_bot_spoke,
        };
        chime_in::is_candidate(&candidate) && chime_in.has_room(now)
    }

    /// Asks the model whether to answer a candidate message, and answers it if so and the limits
    /// still allow. Images aren't shown: the bot wasn't pinged, so they aren't shared with it.
    async fn maybe_chime_in(
        &self,
        rpc: rpc::ProtocolClient,
        message: &Message,
        context: Vec<LlmMessage>,
        mut request_message: LlmMessage,
    ) -> anyhow::Result<()> {
        let Some(chime_in) = &self.chime_in else {
            return Ok(());
        };
        let model = self.llm_model.read().clone();
        let (answering, verdict) = chime_in::should_chime_in(
            &self.llm_client,
            &model,
            message.channel_id,
            &context,
            &request_message,
        )
        .await?;
        tracing::info!(message = %message.link(), "Chime-in check: {verdict}");
        // An answer may have started during the check
        if !answering || chime_in.is_answering() || !chime_in.try_take(Instant::now()) {
            return Ok(());
        }

        request_message
            .content
            .push(ContentPart::Text(TextContentPart {
                text: chime_in::ANSWER_NOTE.to_string(),
            }));
        self.answer_message(rpc, message, context, request_message, &[])
            .await
    }

    /// How a message is kept in the context: from the guild, or a DM from one of its members.
    /// `None` for other messages.
    async fn render_message(
        &self,
        rpc: &rpc::ProtocolClient,
        message: &Message,
    ) -> anyhow::Result<Option<RenderedMessage>> {
        // Messages in the configured guild, and DMs from its members
        let is_dm = message.guild_id.is_none();
        let nick = if is_dm {
            let member = rpc
                .get_member(rpc::context::current(), self.guild_id, message.author.id)
                .await??;
            let Some(member) = member else {
                return Ok(None);
            };
            member.nick
        } else if message.guild_id == Some(self.guild_id) {
            message.member.as_ref().and_then(|m| m.nick.clone())
        } else {
            return Ok(None);
        };

        let user_name = nick.as_deref().unwrap_or(&message.author.name);
        let user_id = message.author.id.get();

        // The guild's nicknames, DMs included
        let content_safe = rpc
            .content_safe(
                rpc::context::current(),
                message.content.clone(),
                Some(self.guild_id),
            )
            .await??
            .replace("@rust-bot", "@globibot");

        let images = MessageImages::of(message);
        let has_role = self.images_role_id.is_some_and(|role| {
            message
                .member
                .as_ref()
                .is_some_and(|member| member.roles.contains(&role))
        });
        let images_consent = (!images.urls.is_empty())
            .then(|| {
                images::consent(
                    is_dm,
                    message.mentions_user_id(self.bot_id),
                    message.author.id,
                    has_role,
                )
            })
            .flatten();
        let stored = {
            let mut texts = vec![format!("{user_name} (<@{user_id}>): {content_safe}")];
            if let Some(replied_to) = &message.referenced_message {
                texts.push(reply_line(replied_to, self.bot_id));
            }
            texts.extend(
                images::gif_descriptions(&message.content)
                    .into_iter()
                    .map(|gif| format!("[GIF: {gif}]")),
            );
            if images.spoilers > 0 {
                texts.push("[Spoiler image]".to_string());
            }
            if !images.urls.is_empty() {
                texts.push(if images_consent.is_some() {
                    images.placeholder().to_string()
                } else {
                    images::NOT_SHARED.to_string()
                });
            }
            ContextMessage {
                id: Some(message.id),
                images_consent,
                message: LlmMessage {
                    role: Role::User,
                    content: texts
                        .into_iter()
                        .map(|text| ContentPart::Text(TextContentPart { text }))
                        .collect(),
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                },
            }
        };

        Ok(Some(RenderedMessage {
            stored,
            user_name: user_name.to_string(),
            images,
            is_dm,
        }))
    }

    async fn register_message(
        &self,
        rpc: rpc::ProtocolClient,
        message: &Message,
        llm_message: ContextMessage,
    ) -> anyhow::Result<()> {
        // Fetch the channel before taking the lock: holding a blocking mutex across an RPC
        // await can starve the runtime's worker threads
        let is_new_channel = !self
            .contexts_by_channel
            .lock()
            .contains_key(&message.channel_id);
        let fetched_channel = if is_new_channel {
            Some(
                rpc.get_channel(rpc::context::current(), message.channel_id)
                    .await??,
            )
        } else {
            None
        };

        let max_size = self.context_window_size.load(Ordering::Relaxed);
        let (dropped, previous_summary) = {
            let mut contexts_by_channel = self.contexts_by_channel.lock();
            let context = match contexts_by_channel.entry(message.channel_id) {
                Entry::Occupied(occupied_entry) => occupied_entry.into_mut(),
                Entry::Vacant(vacant_entry) => {
                    let channel = fetched_channel
                        .context("Channel context vanished while registering message")?;
                    vacant_entry.insert(ChannelContext::new(channel))
                }
            };
            match context.push(llm_message, max_size) {
                Some(dropped) => (dropped, context.summary.clone()),
                None => return Ok(()),
            }
        };

        // Summarizing takes a few seconds: don't hold up the reply
        let llm_client = self.llm_client.clone();
        let model = self.llm_model.read().clone();
        let contexts_by_channel = Arc::clone(&self.contexts_by_channel);
        let channel_id = message.channel_id;
        tokio::spawn(async move {
            let summary = context::summarize(&llm_client, &model, previous_summary, &dropped).await;
            match summary {
                Ok(summary) => {
                    if let Some(context) = contexts_by_channel.lock().get_mut(&channel_id) {
                        context.summary = Some(summary);
                    }
                    tracing::info!(%channel_id, "Summarized {} dropped messages", dropped.len());
                }
                // The dropped messages are lost, the previous summary stays
                Err(err) => tracing::error!(%channel_id, "Failed to summarize context: {err:?}"),
            }
        });

        Ok(())
    }

    fn context_for_channel(&self, chan_id: ChannelId) -> Vec<openrouter::Message> {
        let contexts_by_channel = self.contexts_by_channel.lock();
        let context = contexts_by_channel.get(&chan_id);

        if let Some(context) = context {
            context.prompt_messages()
        } else {
            vec![]
        }
    }

    async fn add_user_context(
        &self,
        ctx: &CommandCtx<'_>,
        AddUserContext {
            user: target_id,
            context,
        }: AddUserContext,
    ) -> anyhow::Result<()> {
        let suggestion_by = ctx
            .interaction
            .member
            .as_deref()
            .map(profile_from_member)
            .context("add-user-context was used outside of a guild")?;

        let suggest_result = self.lore_book.write().suggest_fact(
            target_id,
            suggestion_by,
            &context,
            LoreSource::Command,
        );
        let content = match suggest_result {
            Ok(()) => suggested_addition_reply(target_id),
            Err(why) => why.to_string(),
        };

        Ok(ctx.reply_ephemeral(content).await?)
    }

    async fn add_message_to_lore(
        &self,
        ctx: &CommandCtx<'_>,
        AddToLore { message }: AddToLore,
    ) -> anyhow::Result<()> {
        const MAX_QUOTE_CHARS: usize = 300;

        let author_id = message.author.id;
        let quote = message.content.trim();

        let content = if message.author.bot {
            "Bots don't have lore".to_string()
        } else if quote.is_empty() {
            "That message has no text to add".to_string()
        } else {
            let mut quote = quote.chars().take(MAX_QUOTE_CHARS).collect::<String>();
            if message.content.trim().chars().count() > MAX_QUOTE_CHARS {
                quote.push('…');
            }

            let suggestion_by = ctx
                .interaction
                .member
                .as_deref()
                .map(profile_from_member)
                .context("Add to lore was used outside of a guild")?;

            let suggest_result = self.lore_book.write().suggest_fact(
                author_id,
                suggestion_by,
                &format!("Once said: \"{quote}\""),
                LoreSource::Command,
            );
            match suggest_result {
                Ok(()) => suggested_addition_reply(author_id),
                Err(why) => why.to_string(),
            }
        };

        Ok(ctx.reply_ephemeral(content).await?)
    }

    async fn show_model(&self, ctx: &CommandCtx<'_>) -> anyhow::Result<()> {
        ctx.reply(format!(
            "Current model is set to `{}`",
            self.llm_model.read()
        ))
        .await?;
        Ok(())
    }

    async fn set_model(&self, ctx: &CommandCtx<'_>, new_model: String) -> anyhow::Result<()> {
        if ctx.interaction.user.id != self.admin_id {
            ctx.reply(format!(
                "You do not have permission to change the model. ask <@{}>",
                self.admin_id
            ))
            .await?;
            return Ok(());
        }

        *self.llm_model.write() = new_model.trim().to_string();
        self.save_settings().await;
        ctx.reply(format!("Model changed to `{new_model}`")).await?;

        Ok(())
    }

    async fn show_personality(&self, ctx: &CommandCtx<'_>) -> anyhow::Result<()> {
        ctx.reply(format!(
            "Current personality is set to `{}`",
            self.personality.read().name
        ))
        .await?;
        Ok(())
    }

    async fn set_personality(
        &self,
        ctx: &CommandCtx<'_>,
        choice: PersonalityChoice,
    ) -> anyhow::Result<()> {
        let new_personality_name = choice.value();
        let Some(new_personality) = self
            .personality_registry
            .read()
            .personalities_by_name
            .get(&new_personality_name)
            .cloned()
        else {
            ctx.reply(format!("Unknown personality `{new_personality_name}`"))
                .await?;
            return Ok(());
        };

        *self.personality.write() = new_personality;
        self.save_settings().await;

        ctx.reply(format!("Personality changed to `{new_personality_name}`"))
            .await?;

        Ok(())
    }
}

impl Plugin for LlmPlugin {
    const ID: &'static str = "llm";

    type RpcPolicy = HasRpc<true>;
    type EventsPolicy = HasEvents<true>;
}

/// A message as kept in the context, with what answering it needs
struct RenderedMessage {
    stored: ContextMessage,
    user_name: String,
    images: MessageImages,
    is_dm: bool,
}

impl HandleEvents for LlmPlugin {
    type Err = anyhow::Error;

    async fn on_event(&self, rpc: rpc::ProtocolClient, event: Event) -> Result<(), Self::Err> {
        match event {
            Event::InteractionCreate { interaction } => {
                let command = match LlmCommands::parse(&interaction) {
                    Some(Ok(command)) => command,
                    Some(Err(err)) => {
                        tracing::warn!("Invalid `{}` interaction: {err}", interaction.data.name);
                        return Ok(());
                    }
                    None => return Ok(()),
                };

                let ctx = interaction.ctx(&rpc);
                match command {
                    LlmCommands::Llm(Llm::Model(ModelCommand::Show)) => {
                        self.show_model(&ctx).await?
                    }
                    LlmCommands::Llm(Llm::Model(ModelCommand::Set { model })) => {
                        self.set_model(&ctx, model).await?
                    }
                    LlmCommands::Llm(Llm::Personality(PersonalityCommand::Show)) => {
                        self.show_personality(&ctx).await?
                    }
                    LlmCommands::Llm(Llm::Personality(PersonalityCommand::Set { personality })) => {
                        self.set_personality(&ctx, personality).await?
                    }
                    LlmCommands::Llm(Llm::NightlyLore) => self.nightly_lore_now(&ctx).await?,
                    LlmCommands::AddUserContext(command) => {
                        self.add_user_context(&ctx, command).await?
                    }
                    LlmCommands::AddToLore(command) => {
                        self.add_message_to_lore(&ctx, command).await?
                    }
                    LlmCommands::Roast(Roast { user }) => {
                        self.vibe_check(&ctx, user, Vibe::Roast).await?
                    }
                    LlmCommands::Hype(Hype { user }) => {
                        self.vibe_check(&ctx, user, Vibe::Hype).await?
                    }
                    LlmCommands::Tldr(Tldr { messages }) => self.tldr(&ctx, messages).await?,
                    LlmCommands::LoreTrivia(LoreTrivia) => self.start_lore_trivia(&ctx).await?,
                }
            }

            // Starboard posts come from another bot, so this is checked before bots are ignored
            Event::MessageCreate { message }
                if self.starboard_channel_id == Some(message.channel_id)
                    && message.author.id != self.bot_id =>
            {
                self.comment_on_starboard(rpc, &message).await?
            }

            // Webhook posts, e.g. from a followed announcement channel: kept aside for pings soon
            // after, not in the window
            Event::MessageCreate { message }
                if message.webhook_id.is_some()
                    && message.guild_id == Some(self.guild_id)
                    && !message.embeds.is_empty() =>
            {
                if let Some(context) = self.contexts_by_channel.lock().get_mut(&message.channel_id)
                {
                    context.set_latest_embeds(message.id, embed_post_text(&message));
                }
            }

            Event::MessageCreate { message } if !message.author.bot => {
                let Some(RenderedMessage {
                    stored: stored_message,
                    user_name,
                    images,
                    is_dm,
                }) = self.render_message(&rpc, &message).await?
                else {
                    return Ok(());
                };
                let images_consent = stored_message.images_consent;

                if !is_dm {
                    self.check_trivia_guess(&rpc, &message).await?;
                }

                // Every DM is for the bot
                let mentioned = is_dm || message.mentions_user_id(self.bot_id);
                let answering = mentioned && self.cooldowns.try_start(message.author.id).is_ok();

                // Taken before this message is added, which is sent separately with its images
                let answer_input = if answering {
                    let context = self.context_for_channel(message.channel_id);
                    let embeds = self
                        .contexts_by_channel
                        .lock()
                        .get(&message.channel_id)
                        .and_then(|context| context.recent_embeds().map(str::to_string));
                    let images = self.images_to_show(&rpc, &message, &images).await;
                    Some((context, embeds, images))
                } else {
                    None
                };
                // Not pinged: it may still join in, if the model agrees once it's registered
                let chime_in_context = (!mentioned && self.is_chime_in_candidate(&message))
                    .then(|| self.context_for_channel(message.channel_id));

                self.register_message(rpc.clone(), &message, stored_message.clone())
                    .await?;
                if images_consent.is_some() {
                    self.describe_images(&message, &user_name, &images);
                }

                if let Some((context, embeds, (label, image_urls))) = answer_input {
                    let mut request_message = stored_message.message;
                    if let Some(text) = embeds {
                        request_message
                            .content
                            .push(ContentPart::Text(TextContentPart { text }));
                    }
                    if let Some(label) = label {
                        request_message
                            .content
                            .push(ContentPart::Text(TextContentPart { text: label }));
                    }
                    self.answer_message(rpc, &message, context, request_message, &image_urls)
                        .await?;
                } else if mentioned {
                    // Still kept as context, show why there's no answer
                    rpc.create_reaction(
                        rpc::context::current(),
                        message.channel_id,
                        message.id,
                        ReactionType::Unicode("⏳".to_string()),
                    )
                    .await??;
                } else if let Some(context) = chime_in_context {
                    self.maybe_chime_in(rpc, &message, context, stored_message.message)
                        .await?;
                }
            }

            Event::MessageEdit { message } if !message.author.bot => {
                let Some(rendered) = self.render_message(&rpc, &message).await? else {
                    return Ok(());
                };
                if let Some(context) = self.contexts_by_channel.lock().get_mut(&message.channel_id)
                {
                    context.edit(rendered.stored);
                }
            }

            Event::MessageDelete {
                channel_id,
                message_id,
            } => {
                if let Some(context) = self.contexts_by_channel.lock().get_mut(&channel_id) {
                    context.delete(message_id);
                }
            }

            _ => {}
        }

        Ok(())
    }
}

/// The text of a webhook post: its content, then each embed's title, description and fields.
/// Discord caps embeds at 6000 characters per message.
fn embed_post_text(message: &Message) -> String {
    let mut lines = vec![format!("[Recent post by {}]", message.author.name)];
    lines.push(message.content.clone());
    for embed in &message.embeds {
        lines.extend(embed.title.clone());
        lines.extend(embed.description.clone());
        lines.extend(
            embed
                .fields
                .iter()
                .map(|field| format!("{}: {}", field.name, field.value)),
        );
    }
    lines.retain(|line| !line.trim().is_empty());
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embed_posts_keep_titles_descriptions_and_fields() {
        let message: Message = serde_json::from_value(serde_json::json!({
            "id": "1", "channel_id": "2", "content": "",
            "author": { "id": "3", "username": "Deadlock #announcements", "discriminator": "0000" },
            "timestamp": "2026-11-05T06:10:00Z", "edited_timestamp": null, "tts": false,
            "mention_everyone": false, "mentions": [], "mention_roles": [], "attachments": [],
            "pinned": false, "type": 0, "webhook_id": "4",
            "embeds": [{
                "title": "Minor Update", "description": "- Healbane: nerfed",
                "fields": [{ "name": "Note", "value": "more soon", "inline": false }]
            }]
        }))
        .unwrap();
        assert_eq!(
            embed_post_text(&message),
            "[Recent post by Deadlock #announcements]\nMinor Update\n- Healbane: nerfed\nNote: more soon"
        );
    }

    #[test]
    fn replies_say_who_and_what() {
        let mut replied_to = Message::default();
        replied_to.author.id = UserId::new(7);
        replied_to.author.name = "alice".to_string();
        replied_to.content = "first line\n  second".to_string();
        let bot_id = UserId::new(1);
        assert_eq!(
            reply_line(&replied_to, bot_id),
            "[Replying to alice (<@7>): \"first line second\"]"
        );

        replied_to.author.id = bot_id;
        replied_to.content = String::new();
        assert_eq!(
            reply_line(&replied_to, bot_id),
            "[Replying to a message from you with no text]"
        );

        let attachment = |name: &str, content_type: &str| {
            serde_json::from_value::<globibot_core::serenity::all::Attachment>(serde_json::json!({
                "id": "1", "filename": name, "size": 1, "url": "", "proxy_url": "",
                "content_type": content_type, "width": 10, "height": 10
            }))
            .unwrap()
        };
        replied_to.attachments = vec![attachment("a.png", "image/png")];
        assert_eq!(
            reply_line(&replied_to, bot_id),
            "[Replying to a message from you with 1 image]"
        );
        replied_to.content = "look".to_string();
        replied_to
            .attachments
            .push(attachment("b.png", "image/png"));
        replied_to
            .attachments
            .push(attachment("c.pdf", "application/pdf"));
        assert_eq!(
            reply_line(&replied_to, bot_id),
            "[Replying to you: \"look\", with 2 images and 1 file]"
        );
    }

    #[test]
    fn chat_structure_explains_the_summary_tag() {
        assert!(CHAT_STRUCTURE.contains(context::SUMMARY_TAG));
    }

    fn profile(id: u64) -> DiscordProfile {
        DiscordProfile {
            user_id: UserId::new(id),
            username: format!("user{id}"),
            avatar_url: None,
        }
    }

    /// User 1 with `lore` as older lore
    fn lore_book_with(lore: &str) -> LoreBook {
        let mut lore_book = LoreBook::default();
        let mut user_lore = UserLore::new(profile(1));
        user_lore.lore = lore.to_string();
        lore_book.lore_by_user.insert(UserId::new(1), user_lore);
        lore_book.migrate();
        lore_book
    }

    /// The ids of user 1's pending suggestions, oldest first
    fn suggestion_ids(lore_book: &LoreBook) -> Vec<u64> {
        lore_book.suggestions_by_user[&UserId::new(1)]
            .iter()
            .map(|s| s.id)
            .collect()
    }

    fn user_lore(lore_book: &LoreBook) -> &UserLore {
        &lore_book.lore_by_user[&UserId::new(1)]
    }

    #[test]
    fn older_lore_becomes_a_fact_without_author_or_date() {
        let lore_book = lore_book_with(" Likes cats ");
        let facts = &user_lore(&lore_book).facts;
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].text, "Likes cats");
        assert!(facts[0].suggested_by.is_none() && facts[0].suggested_at.is_none());

        // Not saved as a block again, and empty lore makes no fact
        let saved = serde_json::to_value(user_lore(&lore_book)).unwrap();
        assert!(saved.get("lore").is_none());
        assert!(user_lore(&lore_book_with("")).facts.is_empty());
    }

    #[test]
    fn accepted_facts_are_appended_with_their_author() {
        let mut lore_book = lore_book_with("Likes cats");
        for fact in ["Hates mondays", "Owns a boat"] {
            lore_book
                .suggest_fact(UserId::new(1), profile(2), fact, LoreSource::Web)
                .unwrap();
        }
        let [_, second] = suggestion_ids(&lore_book)[..] else {
            panic!("expected 2 suggestions")
        };
        lore_book.accept(UserId::new(1), second).unwrap();

        let lore = user_lore(&lore_book);
        assert_eq!(lore.text(true), "Likes cats. Owns a boat");
        let fact = lore.facts.last().unwrap();
        assert_eq!(fact.suggested_by.as_ref().unwrap().user_id, UserId::new(2));
        assert!(fact.suggested_at.is_some());
        assert_eq!(fact.source, Some(LoreSource::Web));
        assert_eq!(suggestion_ids(&lore_book).len(), 1);
        assert!(lore_book.accept(UserId::new(1), second).is_err());
    }

    #[test]
    fn edits_rewrite_one_fact_and_replace_the_pending_edit() {
        let mut lore_book = lore_book_with("Likes cats");
        let fact_id = user_lore(&lore_book).facts[0].id;
        lore_book
            .suggest_edit(
                UserId::new(1),
                profile(2),
                fact_id,
                "Likes dogs",
                LoreSource::Web,
            )
            .unwrap();
        lore_book
            .suggest_edit(
                UserId::new(1),
                profile(2),
                fact_id,
                "Likes birds",
                LoreSource::Web,
            )
            .unwrap();
        let unchanged = lore_book.suggest_edit(
            UserId::new(1),
            profile(3),
            fact_id,
            "Likes cats",
            LoreSource::Web,
        );
        assert!(unchanged.is_err());
        assert!(
            lore_book
                .suggest_edit(UserId::new(1), profile(2), 999, "Nope", LoreSource::Web)
                .is_err()
        );

        let ids = suggestion_ids(&lore_book);
        assert_eq!(ids.len(), 1);
        lore_book.accept(UserId::new(1), ids[0]).unwrap();
        let facts = &user_lore(&lore_book).facts;
        assert_eq!(facts.len(), 1);
        assert_eq!(facts[0].text, "Likes birds");
        // Still older lore: an edit keeps who first wrote it
        assert!(facts[0].suggested_by.is_none());
    }

    #[test]
    fn outdated_facts_leave_the_prompt_but_stay_for_the_tools() {
        let mut lore_book = lore_book_with("Lives in Paris");
        lore_book
            .suggest_fact(UserId::new(1), profile(2), "Likes cats", LoreSource::Web)
            .unwrap();
        lore_book
            .accept(UserId::new(1), suggestion_ids(&lore_book)[0])
            .unwrap();
        let paris = user_lore(&lore_book).facts[0].id;
        lore_book.set_outdated(UserId::new(1), paris, true).unwrap();

        assert_eq!(user_lore(&lore_book).text(false), "Likes cats");
        assert_eq!(
            user_lore(&lore_book).text(true),
            "(Outdated) Lives in Paris. Likes cats"
        );
        assert!(!lore_book.to_prompt().contains("Paris"));
        assert!(lore_book.set_outdated(UserId::new(1), 999, true).is_err());
    }

    #[test]
    fn suggestions_are_withdrawn_by_their_author_or_rejected_by_the_admin() {
        let mut lore_book = lore_book_with("");
        for fact in ["Likes cats", "Hates mondays"] {
            lore_book
                .suggest_fact(UserId::new(1), profile(2), fact, LoreSource::Web)
                .unwrap();
        }
        let [first, second] = suggestion_ids(&lore_book)[..] else {
            panic!("expected 2 suggestions")
        };

        assert!(
            lore_book
                .dismiss(UserId::new(1), first, UserId::new(3), false)
                .is_err()
        );
        lore_book
            .dismiss(UserId::new(1), first, UserId::new(2), false)
            .unwrap();
        lore_book
            .dismiss(UserId::new(1), second, UserId::new(3), true)
            .unwrap();
        assert!(suggestion_ids(&lore_book).is_empty());
    }

    #[test]
    fn votes_go_to_one_suggestion_and_not_your_own() {
        let mut lore_book = lore_book_with("");
        for fact in ["Likes cats", "Hates mondays"] {
            lore_book
                .suggest_fact(UserId::new(1), profile(2), fact, LoreSource::Web)
                .unwrap();
        }
        let ids = suggestion_ids(&lore_book);
        lore_book
            .vote(UserId::new(1), ids[1], UserId::new(3), SuggestionVote::Up)
            .unwrap();
        let suggestions = &lore_book.suggestions_by_user[&UserId::new(1)];
        assert!(suggestions[0].votes_by_user_id.is_empty());
        assert_eq!(suggestions[1].votes_by_user_id.len(), 1);

        let own = lore_book.vote(UserId::new(1), ids[1], UserId::new(2), SuggestionVote::Up);
        assert!(own.is_err());
    }

    #[test]
    fn cannot_suggest_for_yourself() {
        let mut lore_book = lore_book_with("Likes cats");
        let result =
            lore_book.suggest_fact(UserId::new(1), profile(1), "Is great", LoreSource::Web);
        assert!(result.is_err());
    }

    #[test]
    fn lore_books_saved_before_facts_still_load() {
        let json = serde_json::json!({
            "lore_by_user": { "1": { "member": profile(1), "lore": "Likes cats" } },
            "suggestions_by_user": { "1": [
                { "id": 4, "member": profile(1), "suggestion": "a", "suggestion_by": profile(2),
                  "votes_by_user_id": {}, "kind": "Append" },
                { "member": profile(1), "suggestion": "b", "suggestion_by": profile(3),
                  "votes_by_user_id": {} }
            ] },
            "last_suggestion_id": 4
        });
        let mut lore_book: LoreBook = serde_json::from_value(json).unwrap();
        lore_book.migrate();

        assert_eq!(suggestion_ids(&lore_book), [4, 5]);
        assert_eq!(user_lore(&lore_book).facts[0].id, 6);
        assert_eq!(user_lore(&lore_book).text(true), "Likes cats");
        assert_eq!(lore_book.last_id, 6);
        let suggestion = &lore_book.suggestions_by_user[&UserId::new(1)][0];
        assert_eq!(suggestion.fact_id, None);
    }

    fn citation(url: &str) -> UrlCitation {
        UrlCitation {
            url: url.to_string(),
            title: String::new(),
        }
    }

    #[test]
    fn source_links_go_under_the_answer_while_they_fit() {
        let sources = [
            "https://a.com",
            "https://b.com/x y",
            "https://c.com",
            "https://d.com",
            "https://e.com",
        ]
        .map(citation);
        assert_eq!(
            with_source_links("Answer", &sources),
            "Answer\n-# [src1](<https://a.com>) [src2](<https://c.com>) [src3](<https://d.com>)"
        );
        assert_eq!(with_source_links("Answer", &[]), "Answer");

        // A full message keeps its text, links are dropped
        let full = "a".repeat(fun::MAX_MESSAGE_CHARS - 10);
        assert_eq!(with_source_links(&full, &sources), full);
    }
}
