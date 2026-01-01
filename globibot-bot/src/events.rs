use futures::{Sink, SinkExt, Stream, StreamExt};
use globibot_core::events::{AcceptError, Event, EventType, accept};
use std::{collections::HashSet, fmt::Display, io, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::broadcast,
    time::timeout,
};
use tracing::{debug, info, warn};

use crate::web::WEB_STATE;

pub trait EventSink = Sink<Event, Error: Display> + Send + Unpin + 'static;

pub async fn run_publisher<S, T>(transports: S, publisher: Publisher) -> io::Result<()>
where
    S: Stream<Item = io::Result<T>>,
    T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let mut transports = std::pin::pin!(transports);

    while let Some(transport) = transports.next().await.transpose()? {
        debug!("About to accept new subscriber");
        match accept(transport).await {
            Ok((request, subscriber)) => {
                let subscriber = publisher.add_subscriber(subscriber, request.events);
                tokio::spawn({
                    let plugin_id = request.id.clone();
                    async move {
                        subscriber.run().await;
                        WEB_STATE.lock().unwrap().remove_plugin(&plugin_id);
                    }
                });
                WEB_STATE
                    .lock()
                    .unwrap()
                    .register_plugin_events(&request.id);
                info!("New event subscriber spawned: '{id}'", id = request.id);
            }
            Err(AcceptError::IO(err)) => {
                warn!("IO error while accepting new subscriber: {}", err);
            }
            Err(AcceptError::HandshakeMissing) => {
                warn!("Subscriber did not send a subscription request");
            }
            Err(AcceptError::HandshakeTimedOut) => {
                warn!("Subscriber did not send a subscription request in time");
            }
        }
    }

    Ok(())
}

#[derive(Debug, Clone)]
struct BroadcastMessage {
    event: Event,
}

#[derive(Debug, Clone)]
pub struct Publisher {
    sender: broadcast::Sender<BroadcastMessage>,
}

#[derive(Debug)]
struct Subscriber<Transport> {
    transport: Transport,
    events: HashSet<EventType>,
    receiver: broadcast::Receiver<BroadcastMessage>,
}

impl<Transport: EventSink> Subscriber<Transport> {
    async fn run(mut self) {
        while let Ok(BroadcastMessage { event }) = self.receiver.recv().await {
            if !self.events.contains(&event.ty()) {
                continue;
            }

            let send_task = timeout(Duration::from_secs(5), self.transport.send(event.clone()));

            match send_task.await {
                Ok(Ok(_)) => {}
                Ok(Err(why)) => {
                    warn!("Failed to send event to subscriber: {why}");
                    return;
                }
                Err(_timed_out) => {
                    warn!("Timed out while sending event to subscriber");
                    return;
                }
            }
        }
    }
}

impl Publisher {
    pub fn new() -> Self {
        Self {
            sender: broadcast::channel(16).0,
        }
    }

    fn add_subscriber<T: EventSink>(
        &self,
        transport: T,
        events: impl IntoIterator<Item = EventType>,
    ) -> Subscriber<T> {
        Subscriber {
            transport,
            events: events.into_iter().collect(),
            receiver: self.sender.subscribe(),
        }
    }

    pub fn broadcast(&self, event: Event) {
        let ty = event.ty();

        match self.sender.send(BroadcastMessage { event }) {
            Ok(count) => debug!("Broadcasted {ty:?} to {count} subscribers"),
            Err(_) => warn!("Failed to broadcast event"),
        }
    }
}
