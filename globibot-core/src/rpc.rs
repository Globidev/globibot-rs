use crate::transport::{frame_transport, FramedRead, FramedStream, FramedWrite};

use futures::{Future, SinkExt, StreamExt, TryFutureExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serenity::model::{
    channel::{Message, ReactionType},
    id::{ChannelId, GuildId, MessageId},
    interactions::application_command::ApplicationCommand,
    prelude::CurrentUser,
};
use std::{error::Error, io, time::Duration};
use tarpc::{
    client,
    server::{self, BaseChannel},
    ClientMessage, Response,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::timeout,
};

pub use tarpc::context;

#[tarpc::service]
pub trait Protocol {
    async fn current_user() -> CurrentUser;

    async fn send_message(chan_id: ChannelId, content: String) -> DiscordApiResult<Message>;
    async fn edit_message(message: Message, new_content: String) -> DiscordApiResult<Message>;
    async fn delete_message(chan_id: ChannelId, message_id: MessageId) -> DiscordApiResult<()>;
    async fn send_file(
        chan_id: ChannelId,
        data: Vec<u8>,
        name: String,
    ) -> DiscordApiResult<Message>;
    async fn start_typing(chan_id: ChannelId) -> DiscordApiResult<()>;
    async fn content_safe(content: String, guild_id: Option<GuildId>) -> DiscordApiResult<String>;

    async fn create_global_command(data: Value) -> DiscordApiResult<ApplicationCommand>;
    async fn edit_global_command(cmd_id: u64, data: Value) -> DiscordApiResult<ApplicationCommand>;

    async fn create_guild_command(
        guild_id: GuildId,
        data: Value,
    ) -> DiscordApiResult<ApplicationCommand>;
    async fn edit_guild_command(
        cmd_id: u64,
        guild_id: GuildId,
        data: Value,
    ) -> DiscordApiResult<ApplicationCommand>;

    async fn create_interaction_response(
        id: u64,
        token: String,
        data: Value,
    ) -> DiscordApiResult<()>;

    async fn edit_interaction_response(token: String, data: Value) -> DiscordApiResult<Message>;

    async fn create_reaction(
        chan_id: ChannelId,
        message_id: MessageId,
        reaction: ReactionType,
    ) -> DiscordApiResult<()>;
}

#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("Discord API error: {0}")]
pub struct DiscordApiError(String);

pub type DiscordApiResult<T> = Result<T, DiscordApiError>;

impl From<serenity::Error> for DiscordApiError {
    fn from(err: serenity::Error) -> Self {
        Self(err.to_string())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeRequest {
    pub id: String,
}

impl HandshakeRequest {
    pub fn new(id: impl Into<String>) -> Self {
        Self { id: id.into() }
    }
}

#[derive(Debug, derive_more::From)]
pub enum AcceptError {
    IO(io::Error),
    HandshakeTimedOut,
    HandshakeMissing,
}

pub async fn connect<T>(
    config: client::Config,
    mut transport: T,
    request: HandshakeRequest,
) -> io::Result<(
    ProtocolClient,
    impl Future<Output = Result<(), Box<dyn Error + Send + Sync>>>,
)>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let mut handshake_transport: FramedWrite<_, _> = frame_transport(&mut transport);
    handshake_transport.send(request).await?;

    let rpc_transport = frame_transport(transport);
    let client::NewClient { client, dispatch } = ProtocolClient::new(config, rpc_transport);
    Ok((client, dispatch.err_into()))
}

type ServerChannelP<T, Req, Resp> =
    BaseChannel<Req, Resp, FramedStream<T, ClientMessage<Req>, Response<Resp>>>;
pub type ServerChannel<T> = ServerChannelP<T, ProtocolRequest, ProtocolResponse>;

pub async fn accept<T>(
    config: server::Config,
    mut transport: T,
) -> Result<(HandshakeRequest, ServerChannel<T>), AcceptError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let mut handshake_transport: FramedRead<_, _> = frame_transport(&mut transport);
    let timed_request_read = timeout(Duration::from_secs(5), handshake_transport.next());
    let request: HandshakeRequest = timed_request_read
        .await
        .map_err(|_timed_out| AcceptError::HandshakeTimedOut)?
        .ok_or(AcceptError::HandshakeMissing)??;

    let rpc_transport = frame_transport(transport);
    let rpc_channel = ServerChannel::new(config, rpc_transport);
    Ok((request, rpc_channel))
}
