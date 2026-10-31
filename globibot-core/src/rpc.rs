pub use crate::transport::AcceptError;
use crate::transport::{FramedStream, FramedWrite, accept_handshake, frame_transport};

use futures::{Future, SinkExt, TryFutureExt};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use serenity::{
    all::{CommandId, InteractionId, Member, UserId},
    model::{
        application::Command,
        channel::{Message, ReactionType},
        id::{ChannelId, GuildId, MessageId},
        prelude::{Channel, CurrentUser, User},
    },
};
use std::{error::Error, io, net::SocketAddr};
use tarpc::{
    ClientMessage, Response, client,
    server::{self, BaseChannel},
};
use tokio::io::{AsyncRead, AsyncWrite};

pub use tarpc::context;

#[tarpc::service]
pub trait Protocol {
    async fn register_web_api(addr: SocketAddr) -> Result<(), RegisterWebApiError>;

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
    async fn upsert_global_command(data: Value) -> DiscordApiResult<Command>;

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

    async fn list_guild_members(guild_id: GuildId) -> DiscordApiResult<Vec<Member>>;
    /// `None` if the user isn't in the guild
    async fn get_member(guild_id: GuildId, user_id: UserId) -> DiscordApiResult<Option<Member>>;

    /// Discord's message search, only in channels `reader` can read. With `public_only`, also
    /// only in channels everyone in the guild can read, for results shown in a public channel.
    /// NSFW channels are left out.
    async fn search_guild_messages(
        guild_id: GuildId,
        reader: UserId,
        public_only: bool,
        search: MessageSearch,
    ) -> DiscordApiResult<MessageSearchResults>;
}

/// Filters of Discord's message search. Empty lists and `None`s don't filter.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MessageSearch {
    /// Words to look for
    pub content: Option<String>,
    pub author_ids: Vec<UserId>,
    /// Messages that mention one of these users
    pub mentions: Vec<UserId>,
    /// Narrows down the channels the reader can read
    pub channel_ids: Vec<ChannelId>,
    /// What messages contain: `image`, `link`, `file`, `video`, `embed`, `poll`, ...
    pub has: Vec<String>,
    /// Only messages after this one, or after the time this id was made at
    pub min_id: Option<MessageId>,
    pub max_id: Option<MessageId>,
    /// Oldest first instead of newest first
    pub oldest_first: bool,
    /// 1 to 25
    pub limit: u8,
    pub offset: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageSearchResults {
    /// Every match, not only the returned ones. Discord says it can be off while messages are
    /// sent or deleted.
    pub total_results: u64,
    pub messages: Vec<Message>,
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

/// Either way a Discord call through the RPC can fail: the RPC itself, or Discord. Lets
/// `rpc.method(..).await??` be used in functions returning it.
#[derive(Debug, thiserror::Error)]
pub enum CallError {
    #[error(transparent)]
    Rpc(#[from] client::RpcError),
    #[error(transparent)]
    Discord(#[from] DiscordApiError),
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

#[derive(Debug, thiserror::Error, Serialize, Deserialize)]
pub enum RegisterWebApiError {
    #[error("Control plane error: {0}")]
    ControlPlaneError(String),
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
    transport: T,
) -> Result<(HandshakeRequest, ServerChannel<T>), AcceptError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let (request, rpc_transport) = accept_handshake(transport).await?;
    let rpc_channel = ServerChannel::new(config, rpc_transport);
    Ok((request, rpc_channel))
}

slotmap::new_key_type! {
    pub struct TypingKey;
}
