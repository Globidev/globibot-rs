use std::{io, sync::Arc, time::Duration};

use futures::{Stream, StreamExt};
use globibot_core::rpc::{self, AcceptError, DiscordApiError, TypingKey};
use globibot_core::serenity::all::{
    CommandId, CommandOption, CreateAttachment, CreateMessage, EditMessage, InteractionId, Typing,
    UserId,
};
use globibot_core::serenity::model::prelude::{Channel as DiscordChannel, User};
use globibot_core::serenity::{
    cache::Cache as DiscordCache,
    http::Http as DiscordHttp,
    model::{
        application::Command,
        channel::{Message, ReactionType},
        id::{ChannelId, GuildId, MessageId},
        prelude::CurrentUser,
    },
    utils::{self, ContentSafeOptions},
};
use serde::Deserialize;
use tarpc::{ChannelError, context::Context, server::Channel};
use tokio::io::{AsyncRead, AsyncWrite};

use rpc::{DiscordApiResult, Protocol, ServerChannel};
use tracing::{debug, info, warn};

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
                let handle_client = respond_to_rpc_client(client, http, cache);
                tokio::spawn(handle_client);
                info!("New RPC client spawned: '{}'", request.id);
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
    http: Arc<DiscordHttp>,
    cache: Arc<DiscordCache>,
) -> Result<(), ChannelError<io::Error>>
where
    Transport: AsyncRead + AsyncWrite,
{
    let server = Server {
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

    info!("Ended connection with RPC client");

    Ok(())
}

#[derive(Clone)]
struct Server {
    discord_http: Arc<DiscordHttp>,
    discord_cache: Arc<DiscordCache>,

    typings: Arc<parking_lot::Mutex<slotmap::SlotMap<TypingKey, Typing>>>,
}

impl Protocol for Server {
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

    async fn start_typing(self, _ctx: Context, chan_id: ChannelId) -> DiscordApiResult<TypingKey> {
        let typing = self.discord_http.start_typing(chan_id);
        let key = self.typings.lock().insert(typing);

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(8)).await;
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
        let cmd_name = cmd_data
            .get("name")
            .ok_or("Missing command name")?
            .as_str()
            .ok_or("Invalid command name")?;

        let existing_commands = self.discord_http.get_global_commands().await?;

        let Some(existing_cmd) = existing_commands
            .into_iter()
            .find(|cmd| cmd.name == cmd_name)
        else {
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
        let cmd_name = cmd_data
            .get("name")
            .ok_or("Missing command name")?
            .as_str()
            .ok_or("Invalid command name")?;

        let existing_commands = self.discord_http.get_guild_commands(guild_id).await?;

        let Some(existing_cmd) = existing_commands
            .into_iter()
            .find(|cmd| cmd.name == cmd_name)
        else {
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
}

fn command_changed(
    existing_cmd: &Command,
    cmd_data: &serde_json::Value,
) -> Result<bool, DiscordApiError> {
    let description_changed = {
        let cmd_description = cmd_data.get("description").and_then(|v| v.as_str());
        cmd_description.is_some_and(|desc| existing_cmd.description != desc)
    };

    let opts_changed = 'opts_changed: {
        let existing_opts = &existing_cmd.options;
        let Some(cmd_opts) = cmd_data.get("options") else {
            break 'opts_changed existing_opts.is_empty();
        };
        let cmd_opts = Vec::<CommandOption>::deserialize(cmd_opts)
            .map_err(|e| DiscordApiError(e.to_string()))?;

        cmd_opts.iter().any(|opt| {
            let Some(o) = existing_opts.iter().find(|o| o.name == opt.name) else {
                return true;
            };
            serde_json::to_string(o).unwrap() != serde_json::to_string(opt).unwrap()
        })
    };

    Ok(description_changed || opts_changed)
}
