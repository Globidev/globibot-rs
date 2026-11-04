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
    async fn message(&self, _ctx: Context, mut new_message: Message) {
        drop_forwards(&mut new_message);
        self.publisher.broadcast(Event::MessageCreate {
            message: Box::new(new_message),
        });
    }

    async fn interaction_create(&self, _ctx: Context, interaction: Interaction) {
        let Some(mut command) = interaction.command() else {
            return;
        };
        command
            .data
            .resolved
            .messages
            .values_mut()
            .for_each(drop_forwards);
        self.publisher.broadcast(Event::InteractionCreate {
            interaction: Box::new(command),
        });
    }

    async fn cache_ready(&self, _ctx: Context, guilds: Vec<GuildId>) {
        tracing::info!("discord cache ready: {} guilds", guilds.len());
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

/// Serenity 0.12.5 writes forwarded messages (`message_snapshots`) in a shape it can't read
/// back, so plugins would fail to decode the whole message. Nothing reads forwards: drop them.
pub fn drop_forwards(message: &mut Message) {
    message.message_snapshots.clear();
    if let Some(replied) = &mut message.referenced_message {
        drop_forwards(replied);
    }
}

pub async fn client(
    token: &str,
    application_id: u64,
    publisher: Publisher,
) -> serenity::Result<Client> {
    Client::builder(
        token,
        GatewayIntents::default()
            .union(GatewayIntents::MESSAGE_CONTENT | GatewayIntents::GUILD_MEMBERS),
    )
    .event_handler(EventHandler { publisher })
    .application_id(application_id.into())
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// A reply to a forwarded message, as Discord sends it
    fn reply_to_forward() -> Message {
        let message = |id: &str| {
            json!({
                "id": id, "channel_id": "20", "content": "",
                "timestamp": "2026-11-04T19:00:00.000000+00:00", "edited_timestamp": null,
                "tts": false, "mention_everyone": false, "mentions": [], "mention_roles": [],
                "attachments": [], "embeds": [], "pinned": false, "type": 0,
                "author": { "id": "1", "username": "alice", "avatar": null, "discriminator": "0" },
            })
        };
        let mut forward = message("30");
        forward["message_snapshots"] = json!([{ "message": {
            "content": "forwarded", "timestamp": "2026-11-04T18:00:00.000000+00:00",
            "edited_timestamp": null, "mentions": [], "attachments": [], "embeds": [], "type": 0,
        }}]);
        let mut reply = message("31");
        reply["referenced_message"] = forward;
        serde_json::from_value(reply).unwrap()
    }

    fn round_trips(message: &Message) -> bool {
        serde_json::from_value::<Message>(serde_json::to_value(message).unwrap()).is_ok()
    }

    #[test]
    fn forwards_are_dropped_so_plugins_can_decode_messages() {
        let mut message = reply_to_forward();
        // Fails once serenity writes snapshots the way it reads them: drop_forwards can then go
        assert!(!round_trips(&message));

        drop_forwards(&mut message);
        assert!(round_trips(&message));
    }
}
