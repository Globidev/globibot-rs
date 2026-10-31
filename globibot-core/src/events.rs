pub use crate::transport::AcceptError;
use crate::transport::{FramedRead, FramedWrite, accept_handshake, frame_transport};

use std::{collections::HashSet, io};

use futures::SinkExt;
use serde::{Deserialize, Serialize};
use serenity::model::{
    application::CommandInteraction,
    channel::Message,
    id::{ChannelId, MessageId},
};
use tokio::io::{AsyncRead, AsyncWrite};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    MessageCreate {
        message: Box<Message>,
    },
    MessageDelete {
        channel_id: ChannelId,
        message_id: MessageId,
    },
    InteractionCreate {
        interaction: Box<CommandInteraction>,
    },
}

#[derive(Debug, Clone, Copy, Hash, Eq, PartialEq, Serialize, Deserialize)]
pub enum EventType {
    MessageCreate,
    MessageDelete,
    InteractionCreate,
}

impl Event {
    pub fn ty(&self) -> EventType {
        match self {
            Event::MessageCreate { .. } => EventType::MessageCreate,
            Event::MessageDelete { .. } => EventType::MessageDelete,
            Event::InteractionCreate { .. } => EventType::InteractionCreate,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandshakeRequest {
    pub id: String,
    pub events: HashSet<EventType>,
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

pub async fn accept<T>(transport: T) -> Result<(HandshakeRequest, EventWrite<T>), AcceptError>
where
    T: AsyncRead + AsyncWrite + Unpin,
{
    accept_handshake(transport).await
}
