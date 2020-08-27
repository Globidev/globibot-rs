use futures::{
    future, lock::Mutex, pin_mut, stream::FuturesUnordered, Sink, SinkExt, Stream, StreamExt,
};
use globibot_core::events::{accept, AcceptError, Event, EventType, EventWrite};
use std::{collections::HashSet, fmt::Display, io, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    time::timeout,
};

pub trait EventSink =
    Sink<Event> + Send + Unpin + 'static where <Self as Sink<Event>>::Error: Display;

type FramedSharedPublisher<T> = SharedPublisher<EventWrite<T>>;

use tracing::{debug, info, warn};

pub async fn run_publisher<S, T>(
    transports: S,
    shared_publisher: FramedSharedPublisher<T>,
) -> io::Result<()>
where
    S: Stream<Item = io::Result<T>>,
    T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    pin_mut!(transports);

    while let Some(transport_result) = transports.next().await {
        let transport = transport_result?;
        debug!("About to accept new subscriber");
        match accept(transport).await {
            Ok((subscription, subscriber)) => {
                info!("new event subscriber: '{id}'", id = subscription.id);
                shared_publisher
                    .lock()
                    .await
                    .add_subscriber(subscriber, subscription.events);
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

pub type SharedPublisher<T> = Arc<Mutex<Publisher<T>>>;

pub struct Publisher<Transport> {
    subscribers: Vec<(Transport, HashSet<EventType>)>,
}

impl<Transport: EventSink> Publisher<Transport> {
    pub fn add_subscriber(
        &mut self,
        transport: Transport,
        events: impl IntoIterator<Item = EventType>,
    ) {
        self.subscribers
            .push((transport, events.into_iter().collect()))
    }

    pub async fn publish(&mut self, event_type: EventType, event: Event) {
        let subscribers_for_event =
            self.subscribers
                .iter_mut()
                .enumerate()
                .filter_map(|(idx, (transport, events))| {
                    events.contains(&event_type).then_some((idx, transport))
                });

        let sends = subscribers_for_event
            .map(move |(idx, transport)| {
                let event = event.clone();
                async move {
                    let timed_send = timeout(Duration::from_secs(5), transport.send(event));
                    match timed_send.await {
                        Ok(Ok(_)) => None,
                        Ok(Err(why)) => {
                            warn!("Failed to send event to subscriber: {}", why);
                            Some(idx)
                        }
                        Err(_timed_out) => {
                            warn!("Timed out while sending event to subscriber");
                            Some(idx)
                        }
                    }
                }
            })
            .collect::<FuturesUnordered<_>>();

        let mut failed_sends = sends
            .filter_map(|send_result| future::ready(send_result))
            .collect::<Vec<_>>()
            .await;

        failed_sends.sort();

        for &idx in failed_sends.iter().rev() {
            self.subscribers.remove(idx);
        }
    }
}

impl<T> Default for Publisher<T> {
    fn default() -> Self {
        Self {
            subscribers: Default::default(),
        }
    }
}
