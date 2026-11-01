mod commands;
mod context;
mod cooldown;
mod fun;
mod image_generation;
mod images;
mod message_search;
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
use commands::{
    AddToLore, AddUserContext, Hype, Llm, LlmCommands, LoreTrivia, ModelCommand, PersonalityChoice,
    PersonalityCommand, Roast, Tldr,
};
use context::{ChannelContext, ContextMessage, SavedContext};
use cooldown::Cooldowns;
use fun::{Vibe, sanitize_llm_output};
use image_generation::{DailyCounts, ImageSettings};
use images::{Consent, MessageImages};
use openrouter::{ContentPart, Message as LlmMessage, Role, TextContentPart, ToolOutput};
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

    let endpoints =
        common::endpoints::tcp_from_env([EventType::MessageCreate, EventType::InteractionCreate])?;

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
        let lore_book = storage
            .get_opt::<LoreBook>("latest")
            .await
            .context("Failed to load LLM lore book")?
            .unwrap_or_default();
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
}

async fn llm_lore_suggest(
    State(llm_plugin): State<Arc<LlmPlugin>>,
    Extension(profile): Extension<DiscordProfile>,
    Json(suggestion_req): Json<LlmLoreSuggestionRequest>,
) -> impl IntoResponse {
    let suggest_result = llm_plugin.lore_book.write().suggest(
        suggestion_req.for_user_id,
        profile,
        &suggestion_req.suggestion,
    );
    if let Err(why) = suggest_result {
        return (StatusCode::BAD_REQUEST, why).into_response();
    }

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

    {
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
    }

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

    {
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
            user_lore.lore = suggestion.applied_to(&user_lore.lore);
        }
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
            if user_lore.lore.trim().is_empty() {
                continue;
            }
            let username = &user_lore.member.username;
            let user_id = user_lore.member.user_id;
            writeln!(prompt, "{username} (<@{user_id}>)").ok();
        }

        prompt
    }

    fn to_prompt(&self) -> String {
        use std::fmt::Write;

        let mut prompt = format!("{LORE_PREMISE}\n");

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

    /// Creates or replaces `suggestion_by`'s suggestion for `for_user_id`'s lore with a complete
    /// new lore
    fn suggest(
        &mut self,
        for_user_id: UserId,
        suggestion_by: DiscordProfile,
        suggestion: &str,
    ) -> Result<(), &'static str> {
        let suggestion = self.check_suggestion(for_user_id, &suggestion_by, suggestion)?;

        match self.pending_suggestion_mut(for_user_id, suggestion_by.user_id) {
            Some(existing_suggestion) => {
                existing_suggestion.suggestion = suggestion.to_string();
                existing_suggestion.kind = SuggestionKind::Replace;
            }
            None => self.push_suggestion(
                for_user_id,
                suggestion_by,
                suggestion,
                SuggestionKind::Replace,
            ),
        }

        Ok(())
    }

    /// Suggests adding `addition` to `for_user_id`'s lore, without replacing what's already there.
    /// If `suggestion_by` already has a pending suggestion for that user, `addition` is added to it
    fn suggest_addition(
        &mut self,
        for_user_id: UserId,
        suggestion_by: DiscordProfile,
        addition: &str,
    ) -> Result<(), &'static str> {
        let addition = self.check_suggestion(for_user_id, &suggestion_by, addition)?;

        match self.pending_suggestion_mut(for_user_id, suggestion_by.user_id) {
            Some(existing_suggestion) => {
                existing_suggestion.suggestion =
                    append_lore(&existing_suggestion.suggestion, addition);
            }
            None => {
                self.push_suggestion(for_user_id, suggestion_by, addition, SuggestionKind::Append)
            }
        }

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

    fn pending_suggestion_mut(
        &mut self,
        for_user_id: UserId,
        by_user_id: UserId,
    ) -> Option<&mut UserLoreSuggestion> {
        self.suggestions_by_user
            .get_mut(&for_user_id)?
            .iter_mut()
            .find(|s| s.suggestion_by.user_id == by_user_id)
    }

    fn push_suggestion(
        &mut self,
        for_user_id: UserId,
        suggestion_by: DiscordProfile,
        suggestion: &str,
        kind: SuggestionKind,
    ) {
        let Some(user_lore) = self.lore_by_user.get(&for_user_id) else {
            return;
        };

        self.suggestions_by_user
            .entry(for_user_id)
            .or_default()
            .push(UserLoreSuggestion {
                member: user_lore.member.clone(),
                suggestion: suggestion.to_string(),
                suggestion_by,
                votes_by_user_id: HashMap::new(),
                kind,
            });
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

    // Defaulted so that lore books stored before this field existed still load
    #[serde(default)]
    kind: SuggestionKind,
}

impl UserLoreSuggestion {
    /// The lore that results from accepting this suggestion on top of `lore`
    fn applied_to(self, lore: &str) -> String {
        match self.kind {
            SuggestionKind::Replace => self.suggestion,
            SuggestionKind::Append => append_lore(lore, &self.suggestion),
        }
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
enum SuggestionKind {
    /// The suggestion is the complete new lore
    #[default]
    Replace,
    /// The suggestion is added to whatever the lore is when it gets accepted
    Append,
}

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
                .or_insert_with(|| UserLore {
                    member: profile,
                    lore: String::new(),
                });
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
                self.system_prompt_with(&names_prompt),
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
        let calls = calls.into_inner();
        // Sent with whichever answer comes, even the retry without tools: they're paid for
        let files = files.into_inner();

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
            send(answer.clone(), files).await??;

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
            context.set_latest_images(author_name.to_string(), urls.clone());
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

        let suggest_result =
            self.lore_book
                .write()
                .suggest_addition(target_id, suggestion_by, &context);
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

            let suggest_result = self.lore_book.write().suggest_addition(
                author_id,
                suggestion_by,
                &format!("Once said: \"{quote}\""),
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

            Event::MessageCreate { message } if !message.author.bot => {
                // Messages in the configured guild, and DMs from its members
                let is_dm = message.guild_id.is_none();
                let nick = if is_dm {
                    let member = rpc
                        .get_member(rpc::context::current(), self.guild_id, message.author.id)
                        .await??;
                    let Some(member) = member else {
                        return Ok(());
                    };
                    member.nick
                } else if message.guild_id == Some(self.guild_id) {
                    message.member.as_ref().and_then(|m| m.nick.clone())
                } else {
                    return Ok(());
                };

                if !is_dm {
                    self.check_trivia_guess(&rpc, &message).await?;
                }

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

                let images = MessageImages::of(&message);
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
                let stored_message = {
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

                // Every DM is for the bot
                let mentioned = is_dm || message.mentions_user_id(self.bot_id);
                let answering = mentioned && self.cooldowns.try_start(message.author.id).is_ok();

                // Taken before this message is added, which is sent separately with its images
                let answer_input = if answering {
                    let context = self.context_for_channel(message.channel_id);
                    Some((context, self.images_to_show(&rpc, &message, &images).await))
                } else {
                    None
                };

                self.register_message(rpc.clone(), &message, stored_message.clone())
                    .await?;
                if images_consent.is_some() {
                    self.describe_images(&message, user_name, &images);
                }

                if let Some((context, (label, image_urls))) = answer_input {
                    let mut request_message = stored_message.message;
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
                }
            }

            _ => {}
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    fn lore_book_with(lore: &str) -> LoreBook {
        let mut lore_book = LoreBook::default();
        lore_book.lore_by_user.insert(
            UserId::new(1),
            UserLore {
                member: profile(1),
                lore: lore.to_string(),
            },
        );
        lore_book
    }

    fn accept(lore_book: &mut LoreBook, by_user_id: u64) {
        let suggestions = lore_book
            .suggestions_by_user
            .get_mut(&UserId::new(1))
            .unwrap();
        let index = suggestions
            .iter()
            .position(|s| s.suggestion_by.user_id == UserId::new(by_user_id))
            .unwrap();
        let suggestion = suggestions.remove(index);
        let user_lore = lore_book.lore_by_user.get_mut(&UserId::new(1)).unwrap();
        user_lore.lore = suggestion.applied_to(&user_lore.lore);
    }

    fn lore(lore_book: &LoreBook) -> &str {
        &lore_book.lore_by_user[&UserId::new(1)].lore
    }

    #[test]
    fn addition_appends_to_lore_at_acceptance_time() {
        let mut lore_book = lore_book_with("Likes cats");
        lore_book
            .suggest_addition(UserId::new(1), profile(2), "Hates mondays")
            .unwrap();
        lore_book
            .suggest(UserId::new(1), profile(3), "Likes dogs.")
            .unwrap();

        accept(&mut lore_book, 3);
        accept(&mut lore_book, 2);

        assert_eq!(lore(&lore_book), "Likes dogs. Hates mondays");
    }

    #[test]
    fn additions_from_the_same_user_accumulate() {
        let mut lore_book = lore_book_with("");
        lore_book
            .suggest_addition(UserId::new(1), profile(2), "Likes cats")
            .unwrap();
        lore_book
            .suggest_addition(UserId::new(1), profile(2), "Hates mondays")
            .unwrap();

        accept(&mut lore_book, 2);

        assert_eq!(lore(&lore_book), "Likes cats. Hates mondays");
    }

    #[test]
    fn cannot_suggest_for_yourself() {
        let mut lore_book = lore_book_with("Likes cats");
        let result = lore_book.suggest_addition(UserId::new(1), profile(1), "Is great");
        assert!(result.is_err());
    }

    #[test]
    fn stored_suggestions_without_kind_still_load_as_replace() {
        let json = serde_json::json!({
            "member": profile(1),
            "suggestion": "Likes dogs",
            "suggestion_by": profile(2),
            "votes_by_user_id": {}
        });
        let suggestion: UserLoreSuggestion = serde_json::from_value(json).unwrap();
        assert_eq!(suggestion.kind, SuggestionKind::Replace);
    }
}
