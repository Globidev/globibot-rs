//! Personalities: system prompts stored in Redis as `llm_personality:<name>`, read when used so
//! changes apply without a restart. Anyone can suggest a new one, an edit or a deletion on the
//! website, everyone votes, and the admin accepts.
//!
//! Each personality can also have a nickname and avatar in the guild ([`PersonalityProfile`]),
//! set by anyone logged in and applied when switching to it.

use std::{
    collections::HashMap,
    hash::{DefaultHasher, Hash, Hasher},
    sync::Arc,
};

use axum::{
    Extension, Json,
    extract::{Query, State},
    http::{StatusCode, header},
    response::{IntoResponse, Response},
};
use base64::Engine;
use globibot_core::{
    rpc,
    serenity::all::{ChannelId, UserId},
    storage::{self, DiscordProfile, RedisStorage, StorageError, StorageValue},
};

use crate::{LlmPlugin, SuggestionVote, unix_now, usage::UsageKind};

/// A personality's system prompt, keyed by its name. Can also be added or edited by hand in
/// Redis.
pub struct PersonalityPrompt {
    pub prompt: String,
}

impl StorageValue for PersonalityPrompt {
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

/// A personality's nickname and avatar in the guild, set by anyone logged in. `None`: the bot's global
/// name or avatar. Its namespace doesn't match `llm_personality:*`, so it's not listed as a
/// personality.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersonalityProfile {
    pub nickname: Option<String>,
    /// A data URI: `data:image/png;base64,...`
    pub avatar: Option<String>,
}

impl StorageValue for PersonalityProfile {
    type Format = storage::JsonFormat;

    const REDIS_NS: &'static str = "llm_personality_profile";
}

/// Discord's limit
const MAX_NICKNAME_CHARS: usize = 32;
/// About 1MB of image. The website resizes uploads to 512x512 PNGs first.
const MAX_AVATAR_CHARS: usize = 1_400_000;
const AVATAR_TYPES: [&str; 3] = ["image/png", "image/jpeg", "image/gif"];

/// The media type and bytes of an avatar's data URI, if it's a supported image
fn decode_avatar(data_uri: &str) -> Option<(&'static str, Vec<u8>)> {
    let (media_type, data) = data_uri.strip_prefix("data:")?.split_once(";base64,")?;
    let media_type = AVATAR_TYPES.into_iter().find(|t| *t == media_type)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    Some((media_type, bytes))
}

/// Changes the website shows as a new URL, so browsers don't keep the old image
fn avatar_version(data_uri: &str) -> String {
    let mut hasher = DefaultHasher::new();
    data_uri.hash(&mut hasher);
    format!("{:x}", hasher.finish())
}

impl LlmPlugin {
    /// Sets the bot's nickname and avatar in the guild to the active personality's
    pub(crate) async fn apply_personality_profile(
        &self,
        rpc: &rpc::ProtocolClient,
    ) -> anyhow::Result<()> {
        let name = self.personality.read().clone();
        let profile = self
            .storage
            .clone()
            .get_opt::<PersonalityProfile>(&name)
            .await?
            .unwrap_or_default();
        rpc.edit_own_member(
            rpc::context::current(),
            self.guild_id,
            profile.nickname.clone(),
            profile.avatar,
        )
        .await??;
        *self.nickname.write() = profile.nickname;
        tracing::info!("Applied the nickname and avatar of personality `{name}`");
        Ok(())
    }
}

/// Every personality in Redis, sorted by name
pub async fn personality_names(storage: &mut RedisStorage) -> Result<Vec<String>, StorageError> {
    let mut names = storage.list_keys::<PersonalityPrompt>("*").await?;
    names.sort();
    Ok(names)
}

const MAX_NAME_CHARS: usize = 32;
const MAX_PROMPT_CHARS: usize = 10_000;
const NO_SUCH_SUGGESTION: &str = "No such suggestion: it may have been accepted already";

/// Pending suggestions. Its namespace doesn't match `llm_personality:*`, so it's not listed as a
/// personality.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersonalitySuggestions {
    last_id: u64,
    suggestions: Vec<PersonalitySuggestion>,
}

impl StorageValue for PersonalitySuggestions {
    type Format = storage::JsonFormat;

    const REDIS_NS: &'static str = "llm_personality_suggestions";
}

const SUGGESTIONS_KEY: &str = "latest";

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PersonalitySuggestion {
    id: u64,
    name: String,
    change: PersonalityChange,
    suggestion_by: DiscordProfile,
    /// Unix seconds
    suggested_at: u64,
    votes_by_user_id: HashMap<UserId, SuggestionVote>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind")]
pub enum PersonalityChange {
    Create { prompt: String },
    Edit { prompt: String },
    Delete,
}

/// Whether `change` to `name` can apply, given the personalities that exist and the active one
fn check_change(
    name: &str,
    change: &PersonalityChange,
    names: &[String],
    current: &str,
) -> Result<(), &'static str> {
    let exists = names.iter().any(|existing| existing == name);
    match change {
        PersonalityChange::Create { .. } if exists => Err("That personality already exists"),
        PersonalityChange::Edit { .. } | PersonalityChange::Delete if !exists => {
            Err("No such personality")
        }
        PersonalityChange::Delete if name == current => {
            Err("The active personality can't be deleted: switch to another one first")
        }
        _ => Ok(()),
    }
}

impl PersonalitySuggestions {
    /// Adds a suggestion, or replaces `by`'s pending one for the same personality (and its votes)
    fn suggest(
        &mut self,
        name: &str,
        change: PersonalityChange,
        by: DiscordProfile,
        names: &[String],
        current: &str,
    ) -> Result<(), &'static str> {
        let name_is_valid = !name.is_empty()
            && name.chars().count() <= MAX_NAME_CHARS
            && name
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
        if !name_is_valid {
            return Err("Names are 1 to 32 lowercase letters, digits or dashes");
        }
        let change = match change {
            PersonalityChange::Create { prompt } => PersonalityChange::Create {
                prompt: check_prompt(&prompt)?,
            },
            PersonalityChange::Edit { prompt } => PersonalityChange::Edit {
                prompt: check_prompt(&prompt)?,
            },
            PersonalityChange::Delete => PersonalityChange::Delete,
        };
        check_change(name, &change, names, current)?;

        let pending = self
            .suggestions
            .iter_mut()
            .find(|s| s.name == name && s.suggestion_by.user_id == by.user_id);
        match pending {
            Some(pending) => {
                pending.change = change;
                pending.suggested_at = unix_now();
                pending.votes_by_user_id.clear();
            }
            None => {
                self.last_id += 1;
                self.suggestions.push(PersonalitySuggestion {
                    id: self.last_id,
                    name: name.to_string(),
                    change,
                    suggestion_by: by,
                    suggested_at: unix_now(),
                    votes_by_user_id: HashMap::new(),
                });
            }
        }
        Ok(())
    }

    /// Sets `voter`'s vote on a suggestion. Not on their own.
    fn vote(&mut self, id: u64, voter: UserId, vote: SuggestionVote) -> Result<(), &'static str> {
        let suggestion = self
            .suggestions
            .iter_mut()
            .find(|s| s.id == id)
            .ok_or(NO_SUCH_SUGGESTION)?;
        if suggestion.suggestion_by.user_id == voter {
            return Err("You cannot vote on your own suggestions");
        }
        suggestion.votes_by_user_id.insert(voter, vote);
        Ok(())
    }

    /// Removes a suggestion to apply it, if it still can. A deletion also drops the other
    /// suggestions for that personality.
    fn take_to_accept(
        &mut self,
        id: u64,
        names: &[String],
        current: &str,
    ) -> Result<PersonalitySuggestion, &'static str> {
        let index = self
            .suggestions
            .iter()
            .position(|s| s.id == id)
            .ok_or(NO_SUCH_SUGGESTION)?;
        let suggestion = &self.suggestions[index];
        check_change(&suggestion.name, &suggestion.change, names, current)?;

        let suggestion = self.suggestions.remove(index);
        if suggestion.change == PersonalityChange::Delete {
            self.suggestions.retain(|s| s.name != suggestion.name);
        }
        Ok(suggestion)
    }

    /// Removes a suggestion: withdrawn by its author, or rejected by the admin
    fn dismiss(&mut self, id: u64, by: UserId, is_admin: bool) -> Result<(), &'static str> {
        let index = self
            .suggestions
            .iter()
            .position(|s| s.id == id)
            .ok_or(NO_SUCH_SUGGESTION)?;
        if !is_admin && self.suggestions[index].suggestion_by.user_id != by {
            return Err("Only its author or the admin can remove a suggestion");
        }
        self.suggestions.remove(index);
        Ok(())
    }
}

/// Returns the trimmed prompt
fn check_prompt(prompt: &str) -> Result<String, &'static str> {
    let prompt = prompt.trim();
    if prompt.is_empty() {
        return Err("The prompt cannot be empty");
    }
    if prompt.chars().count() > MAX_PROMPT_CHARS {
        return Err("The prompt is too long (10000 characters at most)");
    }
    Ok(prompt.to_string())
}

#[derive(Debug, serde::Serialize)]
struct WebPersonalities {
    current: String,
    personalities: Vec<WebPersonality>,
    suggestions: Vec<PersonalitySuggestion>,
    is_admin: bool,
}

#[derive(Debug, serde::Serialize)]
struct WebPersonality {
    name: String,
    prompt: String,
    nickname: Option<String>,
    /// Changes with the avatar, `None` without one. Fetched from `GET /personality/avatar`.
    avatar_version: Option<String>,
}

async fn load_suggestions(
    storage: &mut RedisStorage,
) -> Result<PersonalitySuggestions, StorageError> {
    Ok(storage
        .get_opt::<PersonalitySuggestions>(SUGGESTIONS_KEY)
        .await?
        .unwrap_or_default())
}

async fn load_web_personalities(
    plugin: &LlmPlugin,
    profile: &DiscordProfile,
) -> Result<WebPersonalities, StorageError> {
    let mut storage = plugin.storage.clone();
    let mut personalities = Vec::new();
    for name in personality_names(&mut storage).await? {
        // Deleted since it was listed: left out
        if let Some(PersonalityPrompt { prompt }) = storage.get_opt(&name).await? {
            let profile = storage
                .get_opt::<PersonalityProfile>(&name)
                .await?
                .unwrap_or_default();
            personalities.push(WebPersonality {
                name,
                prompt,
                nickname: profile.nickname,
                avatar_version: profile.avatar.as_deref().map(avatar_version),
            });
        }
    }
    let suggestions = load_suggestions(&mut storage).await?.suggestions;
    Ok(WebPersonalities {
        current: plugin.personality.read().clone(),
        personalities,
        suggestions,
        is_admin: plugin.admin_id == profile.user_id,
    })
}

fn storage_error(err: StorageError) -> Response {
    tracing::error!("Failed to load or save personalities: {err:?}");
    (
        StatusCode::BAD_GATEWAY,
        "Couldn't reach the personality storage",
    )
        .into_response()
}

/// Every endpoint answers with the whole state
async fn respond(plugin: &LlmPlugin, profile: &DiscordProfile) -> Response {
    match load_web_personalities(plugin, profile).await {
        Ok(personalities) => Json(personalities).into_response(),
        Err(err) => storage_error(err),
    }
}

pub async fn web_list(
    State(plugin): State<Arc<LlmPlugin>>,
    Extension(profile): Extension<DiscordProfile>,
) -> Response {
    respond(&plugin, &profile).await
}

#[derive(Debug, serde::Deserialize)]
pub struct WebSwitch {
    name: String,
}

/// Switches the active personality (anyone logged in), and announces it
pub async fn web_switch(
    State(plugin): State<Arc<LlmPlugin>>,
    State(rpc): State<rpc::ProtocolClient>,
    Extension(profile): Extension<DiscordProfile>,
    Json(WebSwitch { name }): Json<WebSwitch>,
) -> Response {
    match personality_names(&mut plugin.storage.clone()).await {
        Ok(names) if names.contains(&name) => {}
        Ok(_) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("Unknown personality `{name}`"),
            )
                .into_response();
        }
        Err(err) => return storage_error(err),
    }

    let changed = *plugin.personality.read() != name;
    *plugin.personality.write() = name.clone();
    plugin.save_settings().await;

    if changed {
        let (plugin, rpc) = (Arc::clone(&plugin), rpc.clone());
        tokio::spawn(async move {
            if let Err(err) = plugin.apply_personality_profile(&rpc).await {
                tracing::warn!("Failed to apply the personality's nickname and avatar: {err:?}");
            }
        });
    }
    if changed && let Some(channel_id) = plugin.personality_announce_channel_id {
        tokio::spawn(announce(
            Arc::clone(&plugin),
            rpc,
            channel_id,
            profile.username.clone(),
            name,
        ));
    }

    respond(&plugin, &profile).await
}

/// Says who switched the personality, then lets the new one introduce itself
async fn announce(
    plugin: Arc<LlmPlugin>,
    rpc: rpc::ProtocolClient,
    channel_id: ChannelId,
    by: String,
    name: String,
) {
    let header = format!("🎭 `{by}` switched my personality to `{name}` on the website");
    let intro = plugin
        .complete_one_shot(
            UsageKind::Command,
            format!(
                "`{by}` just switched your personality on the website. Introduce your new self to \
                 the channel in two or three sentences."
            ),
            Vec::new(),
        )
        .await;
    let text = match intro {
        Ok(intro) => format!("{header}\n{intro}"),
        Err(err) => {
            tracing::warn!("Failed to write the personality intro: {err:?}");
            header
        }
    };
    let text = crate::fun::sanitize_llm_output(&text);
    match rpc
        .send_message(rpc::context::current(), channel_id, text)
        .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(err)) => tracing::warn!("Failed to announce the personality: {err}"),
        Err(err) => tracing::warn!("Failed to announce the personality: {err}"),
    }
}

/// Only logged: the suggestion is saved either way
async fn dm_admin(rpc: rpc::ProtocolClient, admin_id: UserId, text: String) {
    let channel_id = match rpc.dm_channel(rpc::context::current(), admin_id).await {
        Ok(Ok(channel_id)) => channel_id,
        Ok(Err(err)) => return tracing::warn!("Failed to open the admin's DMs: {err}"),
        Err(err) => return tracing::warn!("Failed to open the admin's DMs: {err}"),
    };
    match rpc
        .send_message(rpc::context::current(), channel_id, text)
        .await
    {
        Ok(Ok(_)) => {}
        Ok(Err(err)) => tracing::warn!("Failed to DM the admin: {err}"),
        Err(err) => tracing::warn!("Failed to DM the admin: {err}"),
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct WebSuggest {
    name: String,
    change: PersonalityChange,
}

pub async fn web_suggest(
    State(plugin): State<Arc<LlmPlugin>>,
    State(rpc): State<rpc::ProtocolClient>,
    Extension(profile): Extension<DiscordProfile>,
    Json(WebSuggest { name, change }): Json<WebSuggest>,
) -> Response {
    let _lock = plugin.personality_suggestions_lock.lock().await;
    let mut storage = plugin.storage.clone();
    let loaded = async {
        Ok::<_, StorageError>((
            load_suggestions(&mut storage).await?,
            personality_names(&mut storage).await?,
        ))
    };
    let (mut suggestions, names) = match loaded.await {
        Ok(loaded) => loaded,
        Err(err) => return storage_error(err),
    };
    let current = plugin.personality.read().clone();
    let name = name.trim();
    let what = match &change {
        PersonalityChange::Create { .. } => format!("a new personality `{name}`"),
        PersonalityChange::Edit { .. } => format!("an edit to `{name}`"),
        PersonalityChange::Delete => format!("deleting `{name}`"),
    };
    if let Err(why) = suggestions.suggest(name, change, profile.clone(), &names, &current) {
        return (StatusCode::BAD_REQUEST, why).into_response();
    }
    if let Err(err) = storage.set(SUGGESTIONS_KEY, &suggestions).await {
        return storage_error(err);
    }

    if profile.user_id != plugin.admin_id {
        let text = format!(
            "🎭 `{}` suggested {what}: <https://globibot.com/plugins/llm#personality>",
            profile.username
        );
        tokio::spawn(dm_admin(rpc, plugin.admin_id, text));
    }

    respond(&plugin, &profile).await
}

#[derive(Debug, serde::Deserialize)]
pub struct WebVote {
    suggestion_id: u64,
    vote: SuggestionVote,
}

pub async fn web_vote(
    State(plugin): State<Arc<LlmPlugin>>,
    Extension(profile): Extension<DiscordProfile>,
    Json(WebVote {
        suggestion_id,
        vote,
    }): Json<WebVote>,
) -> Response {
    let _lock = plugin.personality_suggestions_lock.lock().await;
    let mut storage = plugin.storage.clone();
    let mut suggestions = match load_suggestions(&mut storage).await {
        Ok(suggestions) => suggestions,
        Err(err) => return storage_error(err),
    };
    if let Err(why) = suggestions.vote(suggestion_id, profile.user_id, vote) {
        return (StatusCode::BAD_REQUEST, why).into_response();
    }
    if let Err(err) = storage.set(SUGGESTIONS_KEY, &suggestions).await {
        return storage_error(err);
    }

    respond(&plugin, &profile).await
}

#[derive(Debug, serde::Deserialize)]
pub struct WebSuggestionId {
    suggestion_id: u64,
}

/// Applies a suggestion to Redis (the admin only)
pub async fn web_accept(
    State(plugin): State<Arc<LlmPlugin>>,
    Extension(profile): Extension<DiscordProfile>,
    Json(WebSuggestionId { suggestion_id }): Json<WebSuggestionId>,
) -> Response {
    if plugin.admin_id != profile.user_id {
        return (
            StatusCode::FORBIDDEN,
            "You do not have permission to accept personality suggestions",
        )
            .into_response();
    }

    let _lock = plugin.personality_suggestions_lock.lock().await;
    let mut storage = plugin.storage.clone();
    let loaded = async {
        Ok::<_, StorageError>((
            load_suggestions(&mut storage).await?,
            personality_names(&mut storage).await?,
        ))
    };
    let (mut suggestions, names) = match loaded.await {
        Ok(loaded) => loaded,
        Err(err) => return storage_error(err),
    };
    let current = plugin.personality.read().clone();
    let suggestion = match suggestions.take_to_accept(suggestion_id, &names, &current) {
        Ok(suggestion) => suggestion,
        Err(why) => return (StatusCode::BAD_REQUEST, why).into_response(),
    };
    // Applied before the suggestion is removed: a failure leaves it pending
    let applied = match suggestion.change {
        PersonalityChange::Create { prompt } | PersonalityChange::Edit { prompt } => {
            storage
                .set(&suggestion.name, &PersonalityPrompt { prompt })
                .await
        }
        PersonalityChange::Delete => {
            match storage.del::<PersonalityPrompt>(&suggestion.name).await {
                Ok(()) => storage.del::<PersonalityProfile>(&suggestion.name).await,
                Err(err) => Err(err),
            }
        }
    };
    if let Err(err) = applied {
        return storage_error(err);
    }
    if let Err(err) = storage.set(SUGGESTIONS_KEY, &suggestions).await {
        return storage_error(err);
    }

    respond(&plugin, &profile).await
}

/// Withdraws a suggestion (its author) or rejects it (the admin)
pub async fn web_dismiss(
    State(plugin): State<Arc<LlmPlugin>>,
    Extension(profile): Extension<DiscordProfile>,
    Json(WebSuggestionId { suggestion_id }): Json<WebSuggestionId>,
) -> Response {
    let _lock = plugin.personality_suggestions_lock.lock().await;
    let mut storage = plugin.storage.clone();
    let mut suggestions = match load_suggestions(&mut storage).await {
        Ok(suggestions) => suggestions,
        Err(err) => return storage_error(err),
    };
    let is_admin = plugin.admin_id == profile.user_id;
    if let Err(why) = suggestions.dismiss(suggestion_id, profile.user_id, is_admin) {
        return (StatusCode::BAD_REQUEST, why).into_response();
    }
    if let Err(err) = storage.set(SUGGESTIONS_KEY, &suggestions).await {
        return storage_error(err);
    }

    respond(&plugin, &profile).await
}

#[derive(Debug, serde::Deserialize)]
#[serde(tag = "kind")]
pub enum AvatarChange {
    Keep,
    Remove,
    Set { data_uri: String },
}

#[derive(Debug, serde::Deserialize)]
pub struct WebProfile {
    name: String,
    /// Empty or `None`: the bot's global name
    nickname: Option<String>,
    avatar: AvatarChange,
}

/// Sets a personality's nickname and avatar (anyone logged in). Applied to Discord right away if
/// it's the active one.
pub async fn web_set_profile(
    State(plugin): State<Arc<LlmPlugin>>,
    State(rpc): State<rpc::ProtocolClient>,
    Extension(profile): Extension<DiscordProfile>,
    Json(WebProfile {
        name,
        nickname,
        avatar,
    }): Json<WebProfile>,
) -> Response {
    let nickname = nickname
        .map(|nickname| nickname.trim().to_string())
        .filter(|nickname| !nickname.is_empty());
    if nickname
        .as_ref()
        .is_some_and(|nickname| nickname.chars().count() > MAX_NICKNAME_CHARS)
    {
        return (
            StatusCode::BAD_REQUEST,
            "Nicknames are 32 characters at most",
        )
            .into_response();
    }
    if let AvatarChange::Set { data_uri } = &avatar {
        if data_uri.len() > MAX_AVATAR_CHARS {
            return (
                StatusCode::BAD_REQUEST,
                "The avatar is too big (about 1MB at most)",
            )
                .into_response();
        }
        if decode_avatar(data_uri).is_none() {
            return (
                StatusCode::BAD_REQUEST,
                "Avatars are PNG, JPEG or GIF data URIs",
            )
                .into_response();
        }
    }

    let mut storage = plugin.storage.clone();
    let loaded = async {
        Ok::<_, StorageError>((
            personality_names(&mut storage).await?,
            storage.get_opt::<PersonalityProfile>(&name).await?,
        ))
    };
    let (names, saved) = match loaded.await {
        Ok(loaded) => loaded,
        Err(err) => return storage_error(err),
    };
    if !names.contains(&name) {
        return (StatusCode::BAD_REQUEST, "No such personality").into_response();
    }
    let new_profile = PersonalityProfile {
        nickname,
        avatar: match avatar {
            AvatarChange::Keep => saved.and_then(|saved| saved.avatar),
            AvatarChange::Remove => None,
            AvatarChange::Set { data_uri } => Some(data_uri),
        },
    };
    if let Err(err) = storage.set(&name, &new_profile).await {
        return storage_error(err);
    }
    tracing::info!(
        "`{}` set the nickname and avatar of personality `{name}`",
        profile.username
    );

    if *plugin.personality.read() == name
        && let Err(err) = plugin.apply_personality_profile(&rpc).await
    {
        tracing::warn!("Failed to apply the personality's nickname and avatar: {err:?}");
        return (
            StatusCode::BAD_GATEWAY,
            format!("Saved, but Discord refused it: {err}"),
        )
            .into_response();
    }

    respond(&plugin, &profile).await
}

/// Applies the active personality's nickname and avatar again (anyone logged in), after a
/// failure or a change made in Discord
pub async fn web_apply_profile(
    State(plugin): State<Arc<LlmPlugin>>,
    State(rpc): State<rpc::ProtocolClient>,
    Extension(profile): Extension<DiscordProfile>,
) -> Response {
    if let Err(err) = plugin.apply_personality_profile(&rpc).await {
        tracing::warn!("Failed to apply the personality's nickname and avatar: {err:?}");
        return (
            StatusCode::BAD_GATEWAY,
            format!("Discord refused it: {err}"),
        )
            .into_response();
    }

    respond(&plugin, &profile).await
}

#[derive(Debug, serde::Deserialize)]
pub struct WebAvatarQuery {
    name: String,
}

/// A personality's avatar image. Its URL has a version, so it can be cached.
pub async fn web_avatar(
    State(plugin): State<Arc<LlmPlugin>>,
    Query(WebAvatarQuery { name }): Query<WebAvatarQuery>,
) -> Response {
    let saved = plugin
        .storage
        .clone()
        .get_opt::<PersonalityProfile>(&name)
        .await;
    let data_uri = match saved {
        Ok(saved) => saved.and_then(|saved| saved.avatar),
        Err(err) => return storage_error(err),
    };
    match data_uri.as_deref().and_then(decode_avatar) {
        Some((media_type, bytes)) => (
            [
                (header::CONTENT_TYPE, media_type),
                (
                    header::CACHE_CONTROL,
                    "private, max-age=31536000, immutable",
                ),
            ],
            bytes,
        )
            .into_response(),
        None => (StatusCode::NOT_FOUND, "No avatar").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile(id: u64) -> DiscordProfile {
        DiscordProfile {
            user_id: UserId::new(id),
            username: format!("user{id}"),
            avatar_url: None,
        }
    }

    fn create(prompt: &str) -> PersonalityChange {
        PersonalityChange::Create {
            prompt: prompt.to_string(),
        }
    }

    #[test]
    fn avatars_decode() {
        let (media_type, bytes) = decode_avatar("data:image/png;base64,aGk=").unwrap();
        assert_eq!(
            (media_type, bytes.as_slice()),
            ("image/png", b"hi".as_slice())
        );
        assert!(decode_avatar("data:image/svg+xml;base64,aGk=").is_none());
        assert!(decode_avatar("data:image/png;base64,not base64!").is_none());
        assert!(decode_avatar("https://example.com/a.png").is_none());
    }

    #[test]
    fn suggestions_are_checked_and_accepted() {
        let names = vec!["friendly".to_string(), "zoomer".to_string()];
        let mut s = PersonalitySuggestions::default();

        assert!(
            s.suggest("Bad Name", create("p"), profile(1), &names, "friendly")
                .is_err()
        );
        assert!(
            s.suggest("zoomer", create("p"), profile(1), &names, "friendly")
                .is_err()
        );
        assert!(
            s.suggest("new", create("  "), profile(1), &names, "friendly")
                .is_err()
        );
        let delete = PersonalityChange::Delete;
        assert!(
            s.suggest("friendly", delete.clone(), profile(1), &names, "friendly")
                .is_err()
        );
        assert!(
            s.suggest("gone", delete.clone(), profile(1), &names, "friendly")
                .is_err()
        );

        s.suggest("new", create(" p "), profile(1), &names, "friendly")
            .unwrap();
        s.vote(1, UserId::new(2), SuggestionVote::Up).unwrap();
        assert!(s.vote(1, UserId::new(1), SuggestionVote::Up).is_err());
        // The same author again replaces their suggestion and its votes
        s.suggest("new", create("q"), profile(1), &names, "friendly")
            .unwrap();
        assert_eq!(s.suggestions.len(), 1);
        assert!(s.suggestions[0].votes_by_user_id.is_empty());

        let accepted = s.take_to_accept(1, &names, "friendly").unwrap();
        assert_eq!(accepted.change, create("q"));
        assert!(s.suggestions.is_empty());

        // A deletion drops the other suggestions for that personality, but not the active one
        s.suggest("zoomer", delete, profile(1), &names, "friendly")
            .unwrap();
        let edit = PersonalityChange::Edit {
            prompt: "e".to_string(),
        };
        s.suggest("zoomer", edit, profile(2), &names, "friendly")
            .unwrap();
        assert!(s.take_to_accept(2, &names, "zoomer").is_err());
        s.take_to_accept(2, &names, "friendly").unwrap();
        assert!(s.suggestions.is_empty());

        assert!(s.dismiss(99, UserId::new(1), true).is_err());
    }
}
