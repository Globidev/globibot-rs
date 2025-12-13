use crate::events::Publisher;

use globibot_core::events::Event;
use globibot_core::serenity::all::GatewayIntents;
use globibot_core::serenity::{
    self, Client, async_trait,
    client::Context,
    model::{
        application::Interaction,
        channel::Message,
        id::{ChannelId, GuildId, MessageId},
    },
};

struct EventHandler {
    publisher: Publisher,
}

#[async_trait]
impl serenity::client::EventHandler for EventHandler {
    async fn message(&self, _ctx: Context, new_message: Message) {
        self.publisher.broadcast(Event::MessageCreate {
            message: Box::new(new_message),
        });
    }

    async fn interaction_create(&self, _ctx: Context, interaction: Interaction) {
        let Some(command) = interaction.command() else {
            return;
        };
        self.publisher.broadcast(Event::InteractionCreate {
            interaction: Box::new(command),
        });
    }

    async fn cache_ready(&self, _ctx: Context, _guilds: Vec<GuildId>) {
        tracing::info!("CACHE READY!");
    }

    async fn message_delete(
        &self,
        _ctx: Context,
        channel_id: ChannelId,
        message_id: MessageId,
        _gid: Option<GuildId>,
    ) {
        self.publisher.broadcast(Event::MessageDelete {
            channel_id,
            message_id,
        });
    }
}

pub async fn client(
    token: &str,
    publisher: Publisher,
    application_id: u64,
) -> serenity::Result<Client> {
    let event_handler = EventHandler { publisher };

    let discord_client = Client::builder(
        token,
        GatewayIntents::default().union(GatewayIntents::MESSAGE_CONTENT),
    )
    .event_handler(event_handler)
    .application_id(application_id.into())
    .await?;

    Ok(discord_client)
}
