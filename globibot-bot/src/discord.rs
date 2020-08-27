use crate::events::{EventSink, SharedPublisher as SharedEventPublisher};

use globibot_core::events::{Event, EventType};
use serenity::{
    async_trait,
    client::Context,
    model::{
        channel::Message,
        id::{ChannelId, MessageId},
    },
    Client,
};

struct EventHandler<Transport> {
    event_publisher: SharedEventPublisher<Transport>,
}

impl<Transport: EventSink> EventHandler<Transport> {
    async fn publish(&self, event_type: EventType, event: Event) {
        self.event_publisher
            .lock()
            .await
            .publish(event_type, event)
            .await
    }
}

#[async_trait]
impl<Transport: EventSink> serenity::client::EventHandler for EventHandler<Transport> {
    async fn message(&self, _ctx: Context, new_message: Message) {
        self.publish(EventType::MessageCreate, Event::MessageCreate(new_message))
            .await
    }

    async fn message_delete(&self, _ctx: Context, chan_id: ChannelId, message_id: MessageId) {
        self.publish(
            EventType::MessageDelete,
            Event::MessageDelete(chan_id, message_id),
        )
        .await
    }
}

pub async fn client<Transport: EventSink>(
    token: &str,
    event_publisher: SharedEventPublisher<Transport>,
) -> serenity::Result<Client> {
    let event_handler = EventHandler { event_publisher };

    let discord_client = Client::new(token).event_handler(event_handler).await?;

    Ok(discord_client)
}
