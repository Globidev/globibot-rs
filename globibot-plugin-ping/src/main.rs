use globibot_core::{event_stream, EventType, SubscriptionRequest};
use parity_tokio_ipc::Endpoint;

use futures::{lock::Mutex, SinkExt, TryStreamExt};
use std::{collections::HashMap, sync::Arc};
use tarpc::context;

#[tokio::main]
async fn main() {
    let rpc_endpoint = Endpoint::connect("globibot-rpc")
        .await
        .expect("Failed to connect to IPC server");
    let event_endpoint = Endpoint::connect("globibot-events")
        .await
        .expect("Failed to connect to event stream");

    let mut event_stream = event_stream(event_endpoint);
    let events = vec![EventType::MessageCreate, EventType::MessageDelete]
        .into_iter()
        .collect();
    event_stream
        .send(SubscriptionRequest {
            id: "Ping".to_owned(),
            events,
        })
        .await
        .expect("Failed to send subscription request");

    let (rpc_client, dispatch) = globibot_core::rpc_client(Default::default(), rpc_endpoint);

    let pong_map = Arc::new(Mutex::new(HashMap::new()));

    let work = event_stream.try_for_each(move |event| {
        let mut rpc_client = rpc_client.clone();
        let pong_map = Arc::clone(&pong_map);
        async move {
            match event {
                globibot_core::Event::MessageCreate(message) => {
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
                globibot_core::Event::MessageDelete(channel_id, message_id) => {
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
