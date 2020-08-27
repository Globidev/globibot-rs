use globibot_core::{
    events::{self, Event, EventType},
    rpc,
    transport::{Ipc, Protocol, Tcp},
};

use futures::{lock::Mutex, TryStreamExt};
use std::{collections::HashMap, sync::Arc};
use tarpc::context;

const PLUGIN_ID: &str = "Ping";

#[tokio::main]
async fn main() {
    let rpc_endpoint = Tcp::new("127.0.0.1:4243")
        .connect()
        .await
        .expect("Failed to reach RPC endpoint");

    let event_endpoint = Tcp::new("127.0.0.1:4242")
        .connect()
        .await
        .expect("Failed to reach event endpoint");

    let events = vec![EventType::MessageCreate, EventType::MessageDelete];
    let event_stream = events::connect(
        event_endpoint,
        events::HandshakeRequest::new(PLUGIN_ID, events),
    )
    .await
    .expect("Failed to connect to event server");

    let (rpc_client, dispatch) = rpc::connect(
        Default::default(),
        rpc::HandshakeRequest::new(PLUGIN_ID),
        rpc_endpoint,
    )
    .await
    .expect("Failed to connect to RPC server");

    let pong_map = Arc::new(Mutex::new(HashMap::new()));

    let work = event_stream.try_for_each(move |event| {
        let mut rpc_client = rpc_client.clone();
        let pong_map = Arc::clone(&pong_map);
        async move {
            match event {
                Event::MessageCreate(message) => {
                    if message.content.starts_with("!ping") {
                        let orig_message_id = message.id;
                        let message = rpc_client
                            .send_message(context::current(), message.channel_id, "pong!".into())
                            .await
                            .expect("RPC request Failed")
                            .expect("Failed to send message");
                        pong_map.lock().await.insert(orig_message_id, message);
                    }
                }
                Event::MessageDelete(channel_id, message_id) => {
                    if let Some(message) = pong_map.lock().await.get(&message_id) {
                        rpc_client
                            .delete_message(context::current(), channel_id, message.id)
                            .await
                            .expect("RPC request Failed")
                            .expect("Failed to delete message")
                    }
                }
            }
            Ok(())
        }
    });

    let _ = dbg!(futures::join!(work, dispatch));
}
