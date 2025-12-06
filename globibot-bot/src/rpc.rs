use std::{io, sync::Arc, time::Duration};

use futures::{Stream, StreamExt};
use globibot_core::rpc::{self, AcceptError};
use globibot_core::serenity::all::{
    CommandId, CreateAttachment, CreateMessage, EditMessage, InteractionId, UserId,
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
use tarpc::{
    context::Context,
    server::{Channel, ChannelError},
};
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
}

#[tarpc::server]
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
        Ok(chan_id
            .send_message(self.discord_http, CreateMessage::new().content(content))
            .await?)
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

    async fn start_typing(self, _ctx: Context, chan_id: ChannelId) -> DiscordApiResult<()> {
        let typing = self.discord_http.start_typing(chan_id);

        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(8)).await;
            typing.stop();
        });

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
