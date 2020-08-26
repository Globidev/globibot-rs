use futures::{
    future, lock::Mutex, pin_mut, stream::FuturesUnordered, Sink, SinkExt, Stream, StreamExt,
};
use globibot_core::{event_subscriber, Event, EventSubscriber, EventType, SubscriberProtocolError};
use std::{collections::HashSet, io, sync::Arc};
use tokio::io::{AsyncRead, AsyncWrite};

pub trait EventSink = Sink<Event> + Send + Unpin + 'static;

type FramedSharedPublisher<T> = SharedPublisher<EventSubscriber<T>>;

use tracing::{info, warn};

pub async fn run_publisher<S, T>(
    transports: S,
    shared_publisher: FramedSharedPublisher<T>,
) -> io::Result<()>
where
    S: Stream<Item = io::Result<T>>,
    T: AsyncRead + AsyncWrite + Unpin,
{
    pin_mut!(transports);

    while let Some(transport_result) = transports.next().await {
        let transport = transport_result?;
        match event_subscriber(transport).await {
            Ok((subscription, subscriber)) => {
                info!("new event subscriber: '{id}'", id = subscription.id);
                shared_publisher
                    .lock()
                    .await
                    .add_subscriber(subscriber, subscription.events);
            }
            Err(SubscriberProtocolError::IO(err)) => return Err(err),
            Err(SubscriberProtocolError::MissingSubscribtionRequest) => {
                warn!("Subscriber did not send subscription request");
            }
        }
    }

    Ok(())
}

pub type SharedPublisher<T> = Arc<Mutex<Publisher<T>>>;

pub struct Publisher<Transport> {
    subscribers: Vec<(Transport, HashSet<EventType>)>,
}

impl<Transport> Publisher<Transport>
where
    Transport: Sink<Event> + Unpin,
{
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
                    // TODO: Timeout
                    if let Err(_why) = transport.send(event).await {
                        Some(idx)
                    } else {
                        None
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
