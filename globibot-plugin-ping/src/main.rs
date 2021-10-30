#![feature(type_alias_impl_trait)]

use std::{collections::HashMap, error::Error, sync::Arc};

use globibot_core::{
    events::{Event, EventType},
    plugin::{Endpoints, HandleEvents, HasEvents, HasRpc, Plugin, PluginExt},
    rpc, serenity,
    transport::Tcp,
};
use serenity::model::{channel::Message, id::MessageId};

use futures::{lock::Mutex, Future};

#[derive(Default)]
struct PingPlugin {
    message_map: Mutex<HashMap<MessageId, Message>>,
}

impl Plugin for PingPlugin {
    const ID: &'static str = "Ping";

    type RpcPolicy = HasRpc<true>;
    type EventsPolicy = HasEvents<true>;
}

#[tokio::main]
async fn main() {
    let plugin = PingPlugin::default();

    let events = [EventType::MessageCreate, EventType::MessageDelete];

    let subscriber_addr = std::env::var("SUBSCRIBER_ADDR").expect("msg");
    let rpc_addr = std::env::var("RPC_ADDR").expect("msg");

    let endpoints = Endpoints::new()
        .rpc(Tcp::new(rpc_addr))
        .events(Tcp::new(subscriber_addr), &events);

    plugin
        .connect(endpoints)
        .await
        .expect("Failed to connect plugin")
        .handle_events()
        .await
        .expect("Failed to run plugin");
}

impl HandleEvents for PingPlugin {
    type Err = Box<dyn Error>;
    type Future = impl Future<Output = Result<(), Self::Err>>;

    fn on_event(self: Arc<Self>, rpc: rpc::ProtocolClient, event: Event) -> Self::Future {
        async move {
            match event {
                Event::MessageCreate { message } => {
                    if message.content.starts_with("!ping") {
                        let orig_message_id = message.id;
                        let message = rpc
                            .send_message(
                                rpc::context::current(),
                                message.channel_id,
                                "pong!".into(),
                            )
                            .await??;
                        self.message_map
                            .lock()
                            .await
                            .insert(orig_message_id, message);
                    }
                }
                Event::MessageDelete {
                    channel_id,
                    message_id,
                } => {
                    if let Some(message) = self.message_map.lock().await.get(&message_id) {
                        rpc.delete_message(rpc::context::current(), channel_id, message.id)
                            .await??;
                    }
                }
                _ => (),
            }
            Ok(())
        }
    }
}
