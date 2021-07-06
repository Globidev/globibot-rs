use crate::events::{EventSink, SharedPublisher as SharedEventPublisher};

use globibot_core::events::{Event, EventType};
use serenity::{
    async_trait,
    client::Context,
    model::{
        channel::Message,
        id::{ChannelId, GuildId, MessageId},
        interactions::Interaction,
    },
    Client,
};
use tracing::info;

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
        self.publish(
            EventType::MessageCreate,
            Event::MessageCreate {
                message: new_message,
            },
        )
        .await
    }

    async fn interaction_create(&self, _ctx: Context, interaction: Interaction) {
        self.publish(
            EventType::InteractionCreate,
            Event::InteractionCreate {
                interaction: interaction.application_command().unwrap(),
            },
        )
        .await
    }

    async fn cache_ready(&self, _ctx: Context, _guilds: Vec<GuildId>) {
        info!("CACHE READY!");
    }

    async fn message_delete(
        &self,
        _ctx: Context,
        channel_id: ChannelId,
        message_id: MessageId,
        _gid: Option<GuildId>,
    ) {
        self.publish(
            EventType::MessageDelete,
            Event::MessageDelete {
                channel_id,
                message_id,
            },
        )
        .await
    }
}

pub async fn client<Transport: EventSink>(
    token: &str,
    event_publisher: SharedEventPublisher<Transport>,
    application_id: u64,
) -> serenity::Result<Client> {
    let event_handler = EventHandler { event_publisher };

    let discord_client = Client::builder(token)
        .event_handler(event_handler)
        .application_id(application_id)
        .await?;

    Ok(discord_client)
}
