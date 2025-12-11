use crate::transport::{FramedRead, FramedStream, FramedWrite, frame_transport};

use futures::{Future, SinkExt, StreamExt, TryFutureExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serenity::{
    all::{CommandId, InteractionId, UserId},
    model::{
        application::Command,
        channel::{Message, ReactionType},
        id::{ChannelId, GuildId, MessageId},
        prelude::{Channel, CurrentUser, User},
    },
};
use std::{error::Error, io, time::Duration};
use tarpc::{
    ClientMessage, Response, client,
    server::{self, BaseChannel},
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
    async fn send_reply(
        chan_id: ChannelId,
        content: String,
        reference: MessageId,
    ) -> DiscordApiResult<Message>;
    async fn edit_message(message: Message, new_content: String) -> DiscordApiResult<Message>;
    async fn delete_message(chan_id: ChannelId, message_id: MessageId) -> DiscordApiResult<()>;
    async fn send_file(
        chan_id: ChannelId,
        data: Vec<u8>,
        name: String,
    ) -> DiscordApiResult<Message>;
    async fn content_safe(content: String, guild_id: Option<GuildId>) -> DiscordApiResult<String>;

    async fn start_typing(chan_id: ChannelId) -> DiscordApiResult<TypingKey>;
    async fn stop_typing(key: TypingKey) -> DiscordApiResult<()>;

    async fn create_global_command(data: Value) -> DiscordApiResult<Command>;
    async fn edit_global_command(cmd_id: CommandId, data: Value) -> DiscordApiResult<Command>;

    async fn create_guild_command(guild_id: GuildId, data: Value) -> DiscordApiResult<Command>;
    async fn edit_guild_command(
        cmd_id: CommandId,
        guild_id: GuildId,
        data: Value,
    ) -> DiscordApiResult<Command>;
    async fn upsert_guild_command(guild_id: GuildId, data: Value) -> DiscordApiResult<Command>;

    async fn application_commands() -> DiscordApiResult<Vec<Command>>;
    async fn guild_application_commands() -> DiscordApiResult<Vec<Command>>;

    async fn create_interaction_response(
        id: InteractionId,
        token: String,
        data: Value,
    ) -> DiscordApiResult<()>;

    async fn edit_interaction_response(token: String, data: Value) -> DiscordApiResult<Message>;

    async fn create_reaction(
        chan_id: ChannelId,
        message_id: MessageId,
        reaction: ReactionType,
    ) -> DiscordApiResult<()>;

    async fn get_user(user_id: UserId) -> DiscordApiResult<User>;
    async fn get_channel(channel_id: ChannelId) -> DiscordApiResult<Channel>;
}

#[derive(Debug, Clone, Serialize, Deserialize, thiserror::Error)]
#[error("Discord API error: {0}")]
pub struct DiscordApiError(pub String);

pub type DiscordApiResult<T> = Result<T, DiscordApiError>;

impl From<serenity::Error> for DiscordApiError {
    fn from(err: serenity::Error) -> Self {
        Self(err.to_string())
    }
}

impl From<&'_ str> for DiscordApiError {
    fn from(err: &str) -> Self {
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

#[derive(Debug, thiserror::Error)]
pub enum AcceptError {
    #[error("IO error: {0}")]
    IO(#[from] io::Error),

    #[error("Handshake timed out")]
    HandshakeTimedOut,

    #[error("Handshake missing")]
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

slotmap::new_key_type! {
    pub struct TypingKey;
}
