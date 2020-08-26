#![feature(type_alias_impl_trait)]

use futures::{future::Future, Sink, Stream, StreamExt};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serenity::model::id::MessageId;
use server::BaseChannel;
use std::{collections::HashSet, io};
use tarpc::{client, server, ClientMessage, Response};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_serde::{formats::Json, Framed as SerdeFramed};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

pub use serenity::model::{channel::Message, id::ChannelId};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    MessageCreate(Message),
    MessageDelete(ChannelId, MessageId),
}

#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum EventType {
    MessageCreate,
    MessageDelete,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubscriptionRequest {
    pub id: String,
    pub events: HashSet<EventType>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProtocolError(String);
pub type ProtocolResult<T> = Result<T, ProtocolError>;

impl From<serenity::Error> for ProtocolError {
    fn from(err: serenity::Error) -> Self {
        Self(err.to_string())
    }
}

#[tarpc::service]
pub trait Protocol {
    async fn send_message(chan_id: ChannelId, content: String) -> ProtocolResult<Message>;
    async fn delete_message(chan_id: ChannelId, message_id: MessageId) -> ProtocolResult<()>;
}

pub type FramedTransport<T, Req, Resp> =
    SerdeFramed<Framed<T, LengthDelimitedCodec>, Req, Resp, Json<Req, Resp>>;

fn frame_transport<T, Req, Resp>(transport: T) -> FramedTransport<T, Req, Resp>
where
    T: AsyncRead + AsyncWrite,
    Req: Serialize,
    Resp: DeserializeOwned,
{
    let length_framed_transport = Framed::new(transport, LengthDelimitedCodec::new());
    let json_framed_transport = SerdeFramed::new(length_framed_transport, Json::default());
    json_framed_transport
}

pub fn rpc_client<T>(
    config: client::Config,
    transport: T,
) -> (ProtocolClient, impl Future<Output = anyhow::Result<()>>)
where
    T: AsyncRead + AsyncWrite,
{
    let framed_transport = frame_transport(transport);
    let client::NewClient { client, dispatch } = ProtocolClient::new(config, framed_transport);
    (client, dispatch)
}

type ServerChannelP<T, Req, Resp> =
    BaseChannel<Req, Resp, FramedTransport<T, ClientMessage<Req>, Response<Resp>>>;
pub type ServerChannel<T> = ServerChannelP<T, ProtocolRequest, ProtocolResponse>;

pub fn rpc_server<S>(
    config: server::Config,
    transports: S,
) -> impl Stream<Item = ServerChannel<S::Item>>
where
    S: Stream,
    S::Item: AsyncRead + AsyncWrite,
{
    let framed_transports = transports.map(frame_transport);

    let server = server::new(config);
    server.incoming(framed_transports)
}

pub fn accept_rpc_connection<T>(config: server::Config, transport: T) -> ServerChannel<T>
where
    T: AsyncRead + AsyncWrite,
{
    ServerChannel::new(config, frame_transport(transport))
}

pub fn event_stream<T>(transport: T) -> FramedTransport<T, Event, SubscriptionRequest>
where
    T: AsyncRead + AsyncWrite,
{
    frame_transport(transport)
}

pub async fn event_subscriber<T>(
    transport: T,
) -> Result<(SubscriptionRequest, EventSubscriber<T>), SubscriberProtocolError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let mut transport = frame_transport(transport);
    let request = transport
        .next()
        .await
        .ok_or(SubscriberProtocolError::MissingSubscribtionRequest)??;
    Ok((request, transport))
}

pub type EventSubscriber<T> = impl Sink<Event>;

#[derive(Debug, derive_more::From)]
pub enum SubscriberProtocolError {
    IO(io::Error),
    MissingSubscribtionRequest,
}
