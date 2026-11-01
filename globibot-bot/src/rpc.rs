use std::{io, sync::Arc, time::Duration};

use futures::{Stream, StreamExt};
use globibot_core::rpc::{
    self, AcceptError, DiscordApiError, FileAttachment, MessageSearch, MessageSearchResults,
    RegisterWebApiError, TypingKey,
};
use globibot_core::serenity::all::{
    ChannelType, CommandId, CreateAttachment, CreateMessage, EditMessage, Guild, GuildChannel,
    InteractionId, Member, Permissions, RoleId, Typing, UserId,
};
use globibot_core::serenity::model::prelude::{Channel as DiscordChannel, User};
use globibot_core::serenity::{
    cache::Cache as DiscordCache,
    http::{Http as DiscordHttp, HttpError},
    model::{
        application::Command,
        channel::{Message, ReactionType},
        id::{ChannelId, GuildId, MessageId},
        prelude::CurrentUser,
    },
    utils::{self, ContentSafeOptions},
};
use tarpc::{ChannelError, context::Context, server::Channel};
use tokio::io::{AsyncRead, AsyncWrite};

use rpc::{DiscordApiResult, Protocol, ServerChannel};
use tracing::{debug, info, warn};

use crate::web::plugins::REGISTRY;

pub async fn run_server<S, T>(
    transports: S,
    cache: Arc<DiscordCache>,
    http: Arc<DiscordHttp>,
) -> io::Result<()>
where
    S: Stream<Item = io::Result<T>>,
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut transports = std::pin::pin!(transports);

    while let Some(transport_result) = transports.next().await {
        let transport = transport_result?;
        match rpc::accept(Default::default(), transport).await {
            Ok((request, client)) => {
                let http = Arc::clone(&http);
                let cache = Arc::clone(&cache);
                let plugin_id = request.id;
                let handle_client = respond_to_rpc_client(client, plugin_id.clone(), http, cache);
                tokio::spawn({
                    let plugin_id = plugin_id.clone();
                    async move {
                        if let Err(err) = handle_client.await {
                            warn!("RPC client error: {err}");
                        }
                        info!("Ended connection with RPC client: '{plugin_id}'");
                        REGISTRY.lock().unwrap().unregister_rpc(&plugin_id);
                    }
                });
                REGISTRY.lock().unwrap().register_rpc(&plugin_id);
                info!("New RPC client spawned: '{id}'", id = plugin_id);
            }
            Err(AcceptError::IO(err)) => {
                warn!("IO error while accepting new RPC client : {}", err);
            }
            Err(AcceptError::HandshakeMissing) => warn!("RPC client did not send a handshake"),
            Err(AcceptError::HandshakeTimedOut) => {
                warn!("RPC client did not send a handshake in time")
            }
        }
    }

    Ok(())
}

async fn respond_to_rpc_client<Transport>(
    client: ServerChannel<Transport>,
    plugin_id: String,
    http: Arc<DiscordHttp>,
    cache: Arc<DiscordCache>,
) -> Result<(), ChannelError<io::Error>>
where
    Transport: AsyncRead + AsyncWrite,
{
    let server = Server {
        plugin_id,

        discord_http: http,
        discord_cache: cache,

        typings: <_>::default(),
    };

    let serve = server.serve();
    let mut requests = std::pin::pin!(client.requests());

    while let Some(request_result) = requests.next().await {
        debug!("Handling RPC request");
        let request = request_result?;
        request.execute(serve.clone()).await;
    }

    Ok(())
}

#[derive(Clone)]
struct Server {
    plugin_id: String,

    discord_http: Arc<DiscordHttp>,
    discord_cache: Arc<DiscordCache>,

    typings: Arc<parking_lot::Mutex<slotmap::SlotMap<TypingKey, Typing>>>,
}

impl Protocol for Server {
    async fn register_web_api(
        self,
        _ctx: Context,
        addr: std::net::SocketAddr,
    ) -> Result<(), RegisterWebApiError> {
        let caddy_addr = std::env::var("CADDY_ADDR").expect("Missing Caddy address");
        let client = reqwest::Client::new();

        client
            .delete(format!("http://{caddy_addr}/id/route_{}", self.plugin_id))
            .send()
            .await
            .map_err(|err| RegisterWebApiError::ControlPlaneError(err.to_string()))?;

        let payload = serde_json::json!({
            "@id": format!("route_{}", self.plugin_id),
            "match": [{"path": [format!("/{}/*", self.plugin_id)]}],
            "handle": [{
                "handler": "subroute",
                "routes": [
                    {
                        "handle": [
                            {
                                "handler": "rewrite",
                                "strip_path_prefix": format!("/{}", self.plugin_id)
                            }
                        ]
                    },
                    {
                        "handle": [
                            {
                                "handler": "reverse_proxy",
                                "upstreams": [{"dial": addr}]
                            }
                        ]
                    }
                ]
            }],
        });

        client
            .post(format!(
                "http://{caddy_addr}/config/apps/http/servers/srv0/routes"
            ))
            .json(&payload)
            .send()
            .await
            .map_err(|err| RegisterWebApiError::ControlPlaneError(err.to_string()))?
            .error_for_status()
            .map_err(|err| RegisterWebApiError::ControlPlaneError(err.to_string()))?;

        REGISTRY.lock().unwrap().register_web_api(&self.plugin_id);
        Ok(())
    }

    async fn current_user(self, _ctx: Context) -> CurrentUser {
        self.discord_cache.current_user().clone()
    }

    async fn send_message(
        self,
        _ctx: Context,
        chan_id: ChannelId,
        content: String,
    ) -> DiscordApiResult<Message> {
        let message = CreateMessage::new().content(content);
        Ok(chan_id.send_message(self.discord_http, message).await?)
    }

    async fn send_reply(
        self,
        _ctx: Context,
        chan_id: ChannelId,
        content: String,
        reference: MessageId,
    ) -> DiscordApiResult<Message> {
        let message = CreateMessage::new()
            .content(content)
            .reference_message((chan_id, reference));
        Ok(chan_id.send_message(self.discord_http, message).await?)
    }

    async fn delete_message(
        self,
        _ctx: Context,
        chan_id: ChannelId,
        message_id: MessageId,
    ) -> DiscordApiResult<()> {
        Ok(chan_id
            .delete_message(self.discord_http, message_id)
            .await?)
    }

    async fn edit_message(
        self,
        _ctx: Context,
        mut message: Message,
        new_content: String,
    ) -> DiscordApiResult<Message> {
        message
            .edit(self.discord_http, EditMessage::new().content(new_content))
            .await?;
        Ok(message)
    }

    async fn send_file(
        self,
        _ctx: Context,
        chan_id: ChannelId,
        data: Vec<u8>,
        name: String,
    ) -> DiscordApiResult<Message> {
        let attachment = CreateAttachment::bytes(data, name);
        Ok(chan_id
            .send_message(self.discord_http, CreateMessage::new().add_file(attachment))
            .await?)
    }

    async fn send_message_with_files(
        self,
        _ctx: Context,
        chan_id: ChannelId,
        content: String,
        reply_to: Option<MessageId>,
        files: Vec<FileAttachment>,
    ) -> DiscordApiResult<Message> {
        let mut message = CreateMessage::new().content(content).add_files(
            files
                .into_iter()
                .map(|file| CreateAttachment::bytes(file.data, file.name)),
        );
        if let Some(reference) = reply_to {
            message = message.reference_message((chan_id, reference));
        }
        Ok(chan_id.send_message(self.discord_http, message).await?)
    }

    async fn start_typing(self, _ctx: Context, chan_id: ChannelId) -> DiscordApiResult<TypingKey> {
        // Serenity keeps typing until the `Typing` is dropped: by `stop_typing`, or after this in
        // case the plugin never calls it. Answers with several tool rounds can take a minute.
        const MAX_TYPING: Duration = Duration::from_secs(120);

        let typing = self.discord_http.start_typing(chan_id);
        let key = self.typings.lock().insert(typing);

        tokio::spawn(async move {
            tokio::time::sleep(MAX_TYPING).await;
            // Slotmap keys are versioned: a stopped typing's key matches nothing
            self.typings.lock().remove(key);
        });

        Ok(key)
    }

    async fn stop_typing(self, _ctx: Context, key: TypingKey) -> DiscordApiResult<()> {
        let _ = self.typings.lock().remove(key);
        Ok(())
    }

    async fn content_safe(
        self,
        _ctx: Context,
        content: String,
        guild_id: Option<GuildId>,
    ) -> DiscordApiResult<String> {
        let mut opts = ContentSafeOptions::new().show_discriminator(false);
        if let Some(gid) = guild_id {
            opts = opts.display_as_member_from(gid);
        }
        Ok(utils::content_safe(self.discord_cache, content, &opts, &[]))
    }

    async fn create_global_command(
        self,
        _ctx: Context,
        data: serde_json::Value,
    ) -> DiscordApiResult<Command> {
        Ok(self.discord_http.create_global_command(&data).await?)
    }

    async fn edit_global_command(
        self,
        _ctx: Context,
        command_id: CommandId,
        data: serde_json::Value,
    ) -> DiscordApiResult<Command> {
        Ok(self
            .discord_http
            .edit_global_command(command_id, &data)
            .await?)
    }

    async fn upsert_global_command(
        self,
        ctx: Context,
        cmd_data: serde_json::Value,
    ) -> DiscordApiResult<Command> {
        cmd_data
            .get("name")
            .and_then(|name| name.as_str())
            .ok_or("Missing command name")?;

        let existing_commands = self.discord_http.get_global_commands().await?;

        let Some(existing_cmd) = find_command(existing_commands, &cmd_data) else {
            return self.create_global_command(ctx, cmd_data).await;
        };

        if command_changed(&existing_cmd, &cmd_data)? {
            self.edit_global_command(ctx, existing_cmd.id, cmd_data)
                .await
        } else {
            Ok(existing_cmd)
        }
    }

    async fn create_guild_command(
        self,
        _ctx: Context,
        guild_id: GuildId,
        data: serde_json::Value,
    ) -> DiscordApiResult<Command> {
        Ok(self
            .discord_http
            .create_guild_command(guild_id, &data)
            .await?)
    }

    async fn edit_guild_command(
        self,
        _ctx: Context,
        cmd_id: CommandId,
        guild_id: GuildId,
        data: serde_json::Value,
    ) -> DiscordApiResult<Command> {
        Ok(self
            .discord_http
            .edit_guild_command(guild_id, cmd_id, &data)
            .await?)
    }

    async fn upsert_guild_command(
        self,
        ctx: Context,
        guild_id: GuildId,
        cmd_data: serde_json::Value,
    ) -> DiscordApiResult<Command> {
        cmd_data
            .get("name")
            .and_then(|name| name.as_str())
            .ok_or("Missing command name")?;

        let existing_commands = self.discord_http.get_guild_commands(guild_id).await?;

        let Some(existing_cmd) = find_command(existing_commands, &cmd_data) else {
            return self.create_guild_command(ctx, guild_id, cmd_data).await;
        };

        if command_changed(&existing_cmd, &cmd_data)? {
            self.edit_guild_command(ctx, existing_cmd.id, guild_id, cmd_data)
                .await
        } else {
            Ok(existing_cmd)
        }
    }

    async fn application_commands(self, _ctx: Context) -> DiscordApiResult<Vec<Command>> {
        Ok(self.discord_http.get_global_commands().await?)
    }

    async fn guild_application_commands(self, _ctx: Context) -> DiscordApiResult<Vec<Command>> {
        todo!()
    }

    async fn create_interaction_response(
        self,
        _ctx: Context,
        id: InteractionId,
        token: String,
        data: serde_json::Value,
    ) -> DiscordApiResult<()> {
        Ok(self
            .discord_http
            .create_interaction_response(id, &token, &data, vec![])
            .await?)
    }

    async fn edit_interaction_response(
        self,
        _ctx: Context,
        token: String,
        data: serde_json::Value,
    ) -> DiscordApiResult<Message> {
        Ok(self
            .discord_http
            .edit_original_interaction_response(&token, &data, vec![])
            .await?)
    }

    async fn create_reaction(
        self,
        _ctx: Context,
        chan_id: ChannelId,
        message_id: MessageId,
        reaction: ReactionType,
    ) -> DiscordApiResult<()> {
        Ok(chan_id
            .create_reaction(self.discord_http, message_id, reaction)
            .await?)
    }

    async fn get_user(self, _ctx: Context, user_id: UserId) -> DiscordApiResult<User> {
        if let Some(user) = self.discord_cache.user(user_id) {
            return Ok(user.clone());
        }

        Ok(self.discord_http.get_user(user_id).await?)
    }

    async fn get_channel(
        self,
        _ctx: Context,
        channel_id: ChannelId,
    ) -> DiscordApiResult<DiscordChannel> {
        Ok(self.discord_http.get_channel(channel_id).await?)
    }

    async fn dm_channel(self, _ctx: Context, user_id: UserId) -> DiscordApiResult<ChannelId> {
        Ok(user_id.create_dm_channel(&self.discord_http).await?.id)
    }

    async fn list_guild_members(
        self,
        _ctx: Context,
        guild_id: GuildId,
    ) -> DiscordApiResult<Vec<Member>> {
        Ok(self
            .discord_http
            .get_guild_members(guild_id, None, None)
            .await?)
    }

    async fn get_member(
        self,
        _ctx: Context,
        guild_id: GuildId,
        user_id: UserId,
    ) -> DiscordApiResult<Option<Member>> {
        let cached = self
            .discord_cache
            .guild(guild_id)
            .and_then(|guild| guild.members.get(&user_id).cloned());
        if let Some(member) = cached {
            return Ok(Some(member));
        }

        match self.discord_http.get_member(guild_id, user_id).await {
            Ok(member) => Ok(Some(member)),
            Err(globibot_core::serenity::Error::Http(HttpError::UnsuccessfulRequest(response)))
                if response.status_code.as_u16() == 404 =>
            {
                Ok(None)
            }
            Err(err) => Err(err.into()),
        }
    }

    async fn can_send_in(
        self,
        ctx: Context,
        guild_id: GuildId,
        channel_id: ChannelId,
        user_id: UserId,
    ) -> DiscordApiResult<bool> {
        let Some(member) = self.clone().get_member(ctx, guild_id, user_id).await? else {
            return Ok(false);
        };
        let guild = self
            .discord_cache
            .guild(guild_id)
            .ok_or("The server isn't in the cache")?;
        let allowed = match guild.channels.get(&channel_id) {
            Some(channel) => {
                channel.kind != ChannelType::Category
                    && guild
                        .user_permissions_in(channel, &member)
                        .contains(Permissions::VIEW_CHANNEL | Permissions::SEND_MESSAGES)
            }
            // Threads aren't in the channels, and take their permissions from their parent.
            // Private ones also need being added to them: left out.
            None => guild
                .threads
                .iter()
                .find(|thread| thread.id == channel_id && thread.kind == ChannelType::PublicThread)
                .and_then(|thread| guild.channels.get(&thread.parent_id?))
                .is_some_and(|parent| {
                    guild
                        .user_permissions_in(parent, &member)
                        .contains(Permissions::VIEW_CHANNEL | Permissions::SEND_MESSAGES_IN_THREADS)
                }),
        };
        Ok(allowed)
    }

    async fn search_guild_messages(
        self,
        ctx: Context,
        guild_id: GuildId,
        reader: UserId,
        public_only: bool,
        search: MessageSearch,
    ) -> DiscordApiResult<MessageSearchResults> {
        // Discord takes at most this many channels in a search
        const MAX_CHANNELS: usize = 500;

        let Some(member) = self.clone().get_member(ctx, guild_id, reader).await? else {
            return Err("Only members of the server can search it".into());
        };

        let mut channel_ids = {
            let guild = self
                .discord_cache
                .guild(guild_id)
                .ok_or("The server isn't in the cache")?;
            readable_channels(&guild, &member, public_only)
        };
        if !search.channel_ids.is_empty() {
            channel_ids.retain(|id| search.channel_ids.contains(id));
            if channel_ids.is_empty() {
                return Err(if public_only {
                    "Those channels can't be searched from here: only channels everyone can read"
                        .into()
                } else {
                    "Those channels can't be searched: they're not readable by whoever asked".into()
                });
            }
        }
        if channel_ids.len() > MAX_CHANNELS {
            // ponytail: searches the first 500 readable channels, split into several searches if a
            // server ever gets bigger
            warn!(
                "{} readable channels in {guild_id}, searching the first {MAX_CHANNELS}",
                channel_ids.len()
            );
            channel_ids.truncate(MAX_CHANNELS);
        }

        let mut query = vec![
            ("limit", search.limit.clamp(1, 25).to_string()),
            ("offset", search.offset.to_string()),
            (
                "sort_order",
                if search.oldest_first { "asc" } else { "desc" }.to_string(),
            ),
        ];
        query.extend(search.content.map(|content| ("content", content)));
        query.extend(search.min_id.map(|id| ("min_id", id.to_string())));
        query.extend(search.max_id.map(|id| ("max_id", id.to_string())));
        query.extend(
            search
                .author_ids
                .iter()
                .map(|id| ("author_id", id.to_string())),
        );
        query.extend(
            search
                .mentions
                .iter()
                .map(|id| ("mentions", id.to_string())),
        );
        query.extend(search.has.into_iter().map(|has| ("has", has)));
        query.extend(channel_ids.iter().map(|id| ("channel_id", id.to_string())));

        // Serenity doesn't have this endpoint yet
        let response = reqwest::Client::new()
            .get(format!(
                "https://discord.com/api/v10/guilds/{guild_id}/messages/search"
            ))
            .header("Authorization", self.discord_http.token())
            .header("User-Agent", "DiscordBot (https://globibot.com, 1.0)")
            .query(&query)
            .send()
            .await
            .map_err(|err| DiscordApiError(err.to_string()))?;

        let status = response.status();
        if status == reqwest::StatusCode::ACCEPTED {
            return Err("Discord is still indexing this server's messages, try again later".into());
        }
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            return Err(DiscordApiError(format!(
                "Message search failed with {status}: {body}"
            )));
        }

        #[derive(serde::Deserialize)]
        struct SearchResponse {
            total_results: u64,
            /// Each match used to come with the messages around it
            messages: Vec<Vec<Message>>,
        }
        let found: SearchResponse = response
            .json()
            .await
            .map_err(|err| DiscordApiError(err.to_string()))?;

        Ok(MessageSearchResults {
            total_results: found.total_results,
            messages: found.messages.into_iter().flatten().collect(),
        })
    }
}

/// The text channels and active public threads of `guild` that `member` can read, leaving out
/// NSFW ones. With `public_only`, only those the @everyone role can read as well.
#[expect(
    deprecated,
    reason = "role_permissions_in ignores a user's other roles: here it's for @everyone alone"
)]
fn readable_channels(guild: &Guild, member: &Member, public_only: bool) -> Vec<ChannelId> {
    let needed = Permissions::VIEW_CHANNEL | Permissions::READ_MESSAGE_HISTORY;
    // The @everyone role has the guild's id
    let everyone = guild.roles.get(&RoleId::new(guild.id.get()));
    let readable = |channel: &GuildChannel| {
        !channel.nsfw
            && guild.user_permissions_in(channel, member).contains(needed)
            && (!public_only
                || everyone.is_some_and(|role| {
                    guild
                        .role_permissions_in(channel, role)
                        .is_ok_and(|permissions| permissions.contains(needed))
                }))
    };

    let mut channel_ids = guild
        .channels
        .values()
        .filter(|channel| channel.kind != ChannelType::Category && readable(channel))
        .map(|channel| channel.id)
        .collect::<Vec<_>>();
    // A thread's messages are in the thread's own channel. Archived threads aren't in the cache.
    let threads = guild
        .threads
        .iter()
        .filter(|thread| {
            thread.kind == ChannelType::PublicThread
                && thread
                    .parent_id
                    .is_some_and(|parent| channel_ids.contains(&parent))
        })
        .map(|thread| thread.id)
        .collect::<Vec<_>>();
    channel_ids.extend(threads);
    channel_ids
}

/// The existing command with the same name and type as `cmd_data`. Discord allows a slash and a
/// user command to share a name, and it ignores type changes in an edit.
fn find_command(commands: Vec<Command>, cmd_data: &serde_json::Value) -> Option<Command> {
    let name = cmd_data.get("name")?.as_str()?;
    let kind = cmd_data.get("type").and_then(|v| v.as_u64()).unwrap_or(1);
    commands
        .into_iter()
        .find(|cmd| cmd.name == name && u64::from(u8::from(cmd.kind)) == kind)
}

/// Whether Discord's copy of a command differs from what a plugin sends.
fn command_changed(
    existing_cmd: &Command,
    cmd_data: &serde_json::Value,
) -> Result<bool, DiscordApiError> {
    let existing =
        serde_json::to_value(existing_cmd).map_err(|e| DiscordApiError(e.to_string()))?;
    // Compared even when not sent, so dropping every option counts as a change
    let mut cmd_data = cmd_data.clone();
    if let Some(fields) = cmd_data.as_object_mut() {
        fields.entry("options").or_insert(serde_json::json!([]));
    }
    Ok(!same_json(&cmd_data, &existing, false))
}

/// Compares what we send (`ours`) to what Discord has (`theirs`). At the top level only our keys
/// count, since Discord adds its own (`id`, `version`, `integration_types`...). Below it (options,
/// choices) keys from both sides count, so removing a field is a change. Missing, `null`, `false`
/// and empty values are alike: that's how Discord leaves out defaults.
fn same_json(ours: &serde_json::Value, theirs: &serde_json::Value, strict: bool) -> bool {
    use serde_json::Value::{Array, Bool, Null, Number, Object};

    let is_default = |value: &serde_json::Value| match value {
        Null | Bool(false) => true,
        Array(items) => items.is_empty(),
        Object(fields) => fields.is_empty(),
        _ => false,
    };

    match (ours, theirs) {
        (Object(ours), Object(theirs)) => {
            ours.keys()
                .chain(theirs.keys().filter(|_| strict))
                .all(|key| {
                    let ours = ours.get(key).unwrap_or(&Null);
                    let theirs = theirs.get(key).unwrap_or(&Null);
                    same_json(ours, theirs, true)
                })
        }
        (Array(ours), Array(theirs)) => {
            ours.len() == theirs.len()
                && ours
                    .iter()
                    .zip(theirs)
                    .all(|(ours, theirs)| same_json(ours, theirs, true))
        }
        // `5` and `5.0` are the same bound
        (Number(ours), Number(theirs)) => ours.as_f64() == theirs.as_f64(),
        _ => ours == theirs || (is_default(ours) && is_default(theirs)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    /// What Discord returns for `/tldr`: its own fields, defaults left out
    fn discord_tldr() -> Value {
        json!({
            "id": "1", "application_id": "2", "version": "3", "guild_id": "4",
            "type": 1, "name": "tldr", "description": "Summarize",
            "default_member_permissions": null, "dm_permission": true, "nsfw": false,
            "integration_types": [0], "contexts": null,
            "options": [{
                "type": 4, "name": "messages", "description": "How many",
                "min_value": 5, "max_value": 200
            }]
        })
    }

    /// What the plugin sends for `/tldr`
    fn ours_tldr() -> Value {
        json!({
            "type": 1, "name": "tldr", "description": "Summarize",
            "options": [{
                "type": 4, "name": "messages", "description": "How many", "required": false,
                "min_value": 5, "max_value": 200
            }]
        })
    }

    fn changed(discord: Value, ours: Value) -> bool {
        let existing = serde_json::from_value::<Command>(discord).unwrap();
        command_changed(&existing, &ours).unwrap()
    }

    #[test]
    fn same_command_is_unchanged() {
        assert!(!changed(discord_tldr(), ours_tldr()));
    }

    #[test]
    fn detects_changes() {
        let mut ours = ours_tldr();
        ours["description"] = json!("Summarize recent messages");
        assert!(changed(discord_tldr(), ours), "description");

        let mut ours = ours_tldr();
        ours["options"][0]["max_value"] = json!(100);
        assert!(changed(discord_tldr(), ours), "option bound");

        let mut ours = ours_tldr();
        ours["options"][0]
            .as_object_mut()
            .unwrap()
            .remove("min_value");
        assert!(changed(discord_tldr(), ours), "removed option field");

        let mut ours = ours_tldr();
        ours.as_object_mut().unwrap().remove("options");
        assert!(changed(discord_tldr(), ours), "removed every option");
    }

    #[test]
    fn detects_handler_change() {
        let discord = json!({
            "id": "1", "application_id": "2", "version": "3",
            "type": 4, "name": "launch", "description": "Play", "handler": 2
        });
        let ours = json!({ "type": 4, "name": "launch", "description": "Play", "handler": 1 });
        assert!(changed(discord.clone(), ours.clone()));

        let mut same = ours;
        same["handler"] = json!(2);
        assert!(!changed(discord, same));
    }

    #[test]
    fn finds_command_by_name_and_type() {
        let command = |kind: u8| {
            serde_json::from_value::<Command>(json!({
                "id": kind.to_string(), "application_id": "2", "version": "3",
                "type": kind, "name": "crossword", "description": ""
            }))
            .unwrap()
        };
        let commands = || vec![command(1), command(4)];

        let found = find_command(commands(), &json!({ "type": 4, "name": "crossword" }));
        assert_eq!(found.map(|cmd| cmd.id.get()), Some(4));

        // No type means a slash command
        let found = find_command(commands(), &json!({ "name": "crossword" }));
        assert_eq!(found.map(|cmd| cmd.id.get()), Some(1));

        assert!(find_command(commands(), &json!({ "type": 2, "name": "crossword" })).is_none());
    }
}
