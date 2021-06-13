use crate::transport::{frame_transport, FramedRead, FramedWrite};

use std::{collections::HashSet, io, time::Duration};

use futures::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serenity::model::{
    channel::Message,
    id::{ApplicationId, ChannelId, InteractionId, MessageId},
    interactions::ApplicationCommandInteractionData,
    prelude::User,
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::timeout,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    MessageCreate {
        message: Message,
    },
    MessageDelete {
        channel_id: ChannelId,
        message_id: MessageId,
    },
    InteractionCreate {
        interaction: CustomInteraction,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CustomInteraction {
    pub id: InteractionId,
    pub application_id: ApplicationId,
    pub token: String,
    pub data: Option<ApplicationCommandInteractionData>,
    pub channel_id: Option<ChannelId>,
    pub author: Option<User>,
}

#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum EventType {
    MessageCreate,
    MessageDelete,
    InteractionCreate,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeRequest {
    pub id: String,
    pub events: HashSet<EventType>,
}

impl HandshakeRequest {
    pub fn new(id: impl Into<String>, events: impl IntoIterator<Item = EventType>) -> Self {
        Self {
            id: id.into(),
            events: events.into_iter().collect(),
        }
    }
}

#[derive(Debug, derive_more::From)]
pub enum AcceptError {
    IO(io::Error),
    HandshakeTimedOut,
    HandshakeMissing,
}

pub type EventRead<T> = FramedRead<T, Event>;
pub type EventWrite<T> = FramedWrite<T, Event>;

pub async fn connect<T>(mut transport: T, request: HandshakeRequest) -> io::Result<EventRead<T>>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let mut handshake_transport: FramedWrite<_, _> = frame_transport(&mut transport);
    handshake_transport.send(request).await?;

    let rpc_transport: EventRead<T> = frame_transport(transport);
    Ok(rpc_transport)
}

pub async fn accept<T>(mut transport: T) -> Result<(HandshakeRequest, EventWrite<T>), AcceptError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    let mut handshake_transport: FramedRead<_, _> = frame_transport(&mut transport);
    let timed_request_read = timeout(Duration::from_secs(5), handshake_transport.next());

    let request = timed_request_read
        .await
        .map_err(|_timed_out| AcceptError::HandshakeTimedOut)?
        .ok_or(AcceptError::HandshakeMissing)??;

    let rpc_transport: FramedWrite<T, Event> = frame_transport(transport);
    Ok((request, rpc_transport))
}
