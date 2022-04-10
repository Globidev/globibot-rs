use std::{io, sync::Arc, time::Duration};

use futures::{pin_mut, Stream, StreamExt};
use globibot_core::rpc::{self, AcceptError};
use globibot_core::serenity::{
    cache::Cache as DiscordCache,
    http::Http as DiscordHttp,
    model::{
        channel::{Message, ReactionType},
        id::{ChannelId, GuildId, MessageId},
        interactions::application_command::ApplicationCommand,
        prelude::CurrentUser,
    },
    utils::{self, ContentSafeOptions},
    CacheAndHttp,
};
use tarpc::{
    context::Context,
    server::{Channel, ChannelError},
};
use tokio::io::{AsyncRead, AsyncWrite};

use rpc::{Protocol, ProtocolResult, ServerChannel};
use tracing::{debug, info, warn};

pub async fn run_server<S, T>(transports: S, cache_and_http: Arc<CacheAndHttp>) -> io::Result<()>
where
    S: Stream<Item = io::Result<T>>,
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    pin_mut!(transports);

    while let Some(transport_result) = transports.next().await {
        let transport = transport_result?;
        match rpc::accept(Default::default(), transport).await {
            Ok((request, client)) => {
                let http = Arc::clone(&cache_and_http.http);
                let cache = Arc::clone(&cache_and_http.cache);
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
    let requests = client.requests();
    pin_mut!(requests);

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
        self.discord_cache.current_user().await
    }

    async fn send_message(
        self,
        _ctx: Context,
        chan_id: ChannelId,
        content: String,
    ) -> ProtocolResult<Message> {
        Ok(chan_id
            .send_message(self.discord_http, |message| {
                message.content(content);
                message
            })
            .await?)
    }

    async fn delete_message(
        self,
        _ctx: Context,
        chan_id: ChannelId,
        message_id: MessageId,
    ) -> ProtocolResult<()> {
        Ok(chan_id
            .delete_message(self.discord_http, message_id)
            .await?)
    }

    async fn edit_message(
        self,
        _ctx: Context,
        mut message: Message,
        new_content: String,
    ) -> ProtocolResult<Message> {
        message
            .edit(self.discord_http, |message| message.content(new_content))
            .await?;
        Ok(message)
    }

    async fn send_file(
        self,
        _ctx: Context,
        chan_id: ChannelId,
        data: Vec<u8>,
        name: String,
    ) -> ProtocolResult<Message> {
        Ok(chan_id
            .send_message(self.discord_http, |message| {
                message.add_file((data.as_slice(), name.as_str()));
                message
            })
            .await?)
    }

    async fn start_typing(self, _ctx: Context, chan_id: ChannelId) -> ProtocolResult<()> {
        let typing = self
            .discord_http
            .start_typing(chan_id.0)
            .map_err(|e| format!("{}", e))?;

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
    ) -> ProtocolResult<String> {
        let mut opts = ContentSafeOptions::new().show_discriminator(false);
        if let Some(gid) = guild_id {
            opts = opts.display_as_member_from(gid);
        }
        Ok(utils::content_safe(self.discord_cache, content, &opts).await)
    }

    async fn create_global_command(
        self,
        _ctx: Context,
        data: serde_json::Value,
    ) -> ProtocolResult<ApplicationCommand> {
        Ok(self
            .discord_http
            .create_global_application_command(&data)
            .await?)
    }

    async fn edit_global_command(
        self,
        _ctx: Context,
        application_id: u64,
        data: serde_json::Value,
    ) -> ProtocolResult<ApplicationCommand> {
        Ok(self
            .discord_http
            .edit_global_application_command(application_id, &data)
            .await?)
    }

    async fn create_guild_command(
        self,
        _ctx: Context,
        guild_id: GuildId,
        data: serde_json::Value,
    ) -> ProtocolResult<ApplicationCommand> {
        Ok(self
            .discord_http
            .create_guild_application_command(guild_id.0, &data)
            .await?)
    }

    async fn edit_guild_command(
        self,
        _ctx: Context,
        cmd_id: u64,
        guild_id: GuildId,
        data: serde_json::Value,
    ) -> ProtocolResult<ApplicationCommand> {
        Ok(self
            .discord_http
            .edit_guild_application_command(cmd_id, guild_id.0, &data)
            .await?)
    }

    async fn create_interaction_response(
        self,
        _ctx: Context,
        id: u64,
        token: String,
        data: serde_json::Value,
    ) -> ProtocolResult<()> {
        Ok(self
            .discord_http
            .create_interaction_response(id, &token, &data)
            .await?)
    }

    async fn edit_interaction_response(
        self,
        _ctx: Context,
        token: String,
        data: serde_json::Value,
    ) -> ProtocolResult<Message> {
        Ok(self
            .discord_http
            .edit_original_interaction_response(&token, &data)
            .await?)
    }

    async fn create_reaction(
        self,
        _ctx: Context,
        chan_id: ChannelId,
        message_id: MessageId,
        reaction: ReactionType,
    ) -> ProtocolResult<()> {
        Ok(chan_id
            .create_reaction(self.discord_http, message_id, reaction)
            .await?)
    }
}
