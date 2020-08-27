use crate::transport::{frame_transport, FramedRead, FramedStream, FramedWrite};

use futures::{Future, SinkExt, StreamExt, TryFutureExt};
use serde::{Deserialize, Serialize};
use serenity::model::{
    channel::Message,
    id::{ChannelId, MessageId},
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

#[tarpc::service]
pub trait Protocol {
    async fn send_message(chan_id: ChannelId, content: String) -> ProtocolResult<Message>;
    async fn delete_message(chan_id: ChannelId, message_id: MessageId) -> ProtocolResult<()>;
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolError(String);
pub type ProtocolResult<T> = Result<T, ProtocolError>;

impl From<serenity::Error> for ProtocolError {
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
    request: HandshakeRequest,
    mut transport: T,
) -> io::Result<(
    ProtocolClient,
    impl Future<Output = Result<(), Box<dyn Error>>>,
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
