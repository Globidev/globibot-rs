use std::{collections::HashMap, error::Error};

use globibot_core::{
    events::{Event, EventType},
    plugin::{HandleEvents, HasEvents, HasRpc, Plugin},
    rpc, serenity,
};
use serenity::model::{channel::Message, id::MessageId};

#[tokio::main]
async fn main() -> common::anyhow::Result<()> {
    let plugin = PingPlugin::default();

    let endpoints =
        common::endpoints::tcp_from_env([EventType::MessageCreate, EventType::MessageDelete])?;

    plugin.connect(endpoints).await?.handle_events().await?;

    Ok(())
}

#[derive(Default)]
struct PingPlugin {
    message_map: parking_lot::Mutex<HashMap<MessageId, Message>>,
}

impl Plugin for PingPlugin {
    const ID: &'static str = "Ping";

    type RpcPolicy = HasRpc<true>;
    type EventsPolicy = HasEvents<true>;
}

impl HandleEvents for PingPlugin {
    type Err = Box<dyn Error>;

    async fn on_event(&self, rpc: rpc::ProtocolClient, event: Event) -> Result<(), Self::Err> {
        match event {
            Event::MessageCreate { message } if message.content.starts_with("!ping") => {
                let orig_message_id = message.id;
                let message = rpc
                    .send_message(rpc::context::current(), message.channel_id, "pong!".into())
                    .await??;
                self.message_map.lock().insert(orig_message_id, message);
            }
            Event::MessageDelete {
                channel_id,
                message_id,
            } => {
                let Some(&Message { id, .. }) = self.message_map.lock().get(&message_id) else {
                    return Ok(());
                };

                rpc.delete_message(rpc::context::current(), channel_id, id)
                    .await??;
            }
            _ => (),
        }
        Ok(())
    }
}
