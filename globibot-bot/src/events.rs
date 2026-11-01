//! Sends Discord events to the plugins subscribed to them.
//!
//! Recent events are kept in a log, and each plugin has a cursor in it, kept across
//! reconnections by plugin id. A plugin that connects late (the bot just started) or restarts
//! gets the events it missed. Events from while the bot itself was down are lost: Discord
//! doesn't replay them to a new session.

use futures::{Sink, SinkExt, Stream, StreamExt};
use globibot_core::events::{AcceptError, Event, EventType, accept};
use std::{
    collections::{HashMap, HashSet, VecDeque},
    fmt::Display,
    io,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::watch,
    time::timeout,
};
use tracing::{debug, info, warn};

use crate::web::plugins::REGISTRY;

/// Long enough to cover a plugin restart, short enough that late answers still make sense
const KEPT_FOR: Duration = Duration::from_secs(60);
const MAX_KEPT: usize = 1_000;
const INTERACTION_MAX_AGE: Duration = Duration::from_millis(2_500);

pub trait EventTransport = Sink<Event, Error: Display> + Stream + Send + Unpin + 'static;

#[derive(Debug, Default)]
struct Log {
    /// Sequence number of `events[0]`
    first_seq: u64,
    events: VecDeque<(Instant, Event)>,
    /// Per plugin id, the sequence number of the next event to send it
    cursors: HashMap<String, u64>,
}

impl Log {
    fn end_seq(&self) -> u64 {
        self.first_seq + self.events.len() as u64
    }

    fn push(&mut self, event: Event, now: Instant) {
        self.events.push_back((now, event));
        self.prune(now);
    }

    fn prune(&mut self, now: Instant) {
        while self.events.len() > MAX_KEPT
            || self
                .events
                .front()
                .is_some_and(|(at, _)| now.duration_since(*at) > KEPT_FOR)
        {
            self.events.pop_front();
            self.first_seq += 1;
        }
    }

    /// Where `plugin_id` resumes: after the last event it got, or every kept event the first
    /// time it connects to this bot
    fn cursor(&self, plugin_id: &str) -> u64 {
        self.cursors
            .get(plugin_id)
            .copied()
            .unwrap_or(self.first_seq)
    }

    /// The next event at or after `cursor` among `events`, with its sequence number. Moves
    /// `cursor` past it and past the events skipped on the way.
    fn next(
        &mut self,
        cursor: &mut u64,
        events: &HashSet<EventType>,
        now: Instant,
    ) -> Option<(u64, Event)> {
        self.prune(now);
        if *cursor < self.first_seq {
            warn!(
                "A subscriber missed {} events, dropped before it could get them",
                self.first_seq - *cursor
            );
            *cursor = self.first_seq;
        }
        while *cursor < self.end_seq() {
            let seq = *cursor;
            *cursor += 1;
            let (at, event) = &self.events[(seq - self.first_seq) as usize];
            if events.contains(&event.ty()) && !expired(event.ty(), now.duration_since(*at)) {
                return Some((seq, event.clone()));
            }
        }
        None
    }
}

/// Discord wants an answer to an interaction within 3s: older ones would only fail
fn expired(ty: EventType, age: Duration) -> bool {
    ty == EventType::InteractionCreate && age > INTERACTION_MAX_AGE
}

#[derive(Debug, Clone)]
pub struct Publisher {
    log: Arc<Mutex<Log>>,
    /// The log's end sequence number, to wake up subscribers
    latest: watch::Sender<u64>,
}

struct Subscriber<Transport> {
    plugin_id: String,
    transport: Transport,
    events: HashSet<EventType>,
    publisher: Publisher,
}

impl<Transport: EventTransport> Subscriber<Transport> {
    async fn run(mut self) {
        let mut latest = self.publisher.latest.subscribe();
        let mut cursor = self.publisher.log().cursor(&self.plugin_id);
        loop {
            latest.borrow_and_update();
            loop {
                let next = self
                    .publisher
                    .log()
                    .next(&mut cursor, &self.events, Instant::now());
                let Some((seq, event)) = next else { break };

                match timeout(Duration::from_secs(5), self.transport.send(event)).await {
                    Ok(Ok(())) => {}
                    Ok(Err(why)) => {
                        warn!("Failed to send event to '{}': {why}", self.plugin_id);
                        return;
                    }
                    Err(_timed_out) => {
                        warn!("Timed out while sending event to '{}'", self.plugin_id);
                        return;
                    }
                }
                // Only once sent, so a failed send is retried when the plugin reconnects
                self.publisher
                    .log()
                    .cursors
                    .insert(self.plugin_id.clone(), seq + 1);
            }

            tokio::select! {
                changed = latest.changed() => if changed.is_err() { return },
                // Plugins never send anything after the handshake: this is the connection closing
                _ = self.transport.next() => return,
            }
        }
    }
}

impl Publisher {
    pub fn new() -> Self {
        Self {
            log: Arc::default(),
            latest: watch::channel(0).0,
        }
    }

    fn log(&self) -> std::sync::MutexGuard<'_, Log> {
        self.log.lock().unwrap()
    }

    pub fn broadcast(&self, event: Event) {
        let ty = event.ty();
        let end_seq = {
            let mut log = self.log();
            log.push(event, Instant::now());
            log.end_seq()
        };
        self.latest.send_replace(end_seq);
        debug!("Published {ty:?} (#{})", end_seq - 1);
    }

    pub async fn run<S, T>(self, transports: S) -> io::Result<()>
    where
        S: Stream<Item = io::Result<T>>,
        T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let mut transports = std::pin::pin!(transports);

        while let Some(transport) = transports.next().await.transpose()? {
            debug!("About to accept new subscriber");
            match accept(transport).await {
                Ok((request, transport)) => {
                    let missed = {
                        let log = self.log();
                        let from = log.cursor(&request.id).saturating_sub(log.first_seq);
                        log.events
                            .iter()
                            .skip(from as usize)
                            .filter(|(_, event)| request.events.contains(&event.ty()))
                            .count()
                    };
                    let subscriber = Subscriber {
                        plugin_id: request.id.clone(),
                        transport,
                        events: request.events,
                        publisher: self.clone(),
                    };
                    tokio::spawn({
                        let plugin_id = request.id.clone();
                        async move {
                            subscriber.run().await;
                            info!("Ended connection with event subscriber: '{plugin_id}'");
                        }
                    });
                    REGISTRY.lock().unwrap().register_events(&request.id);
                    info!(
                        "New event subscriber spawned: '{id}', {missed} kept event(s) to catch up on",
                        id = request.id
                    );
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
}

#[cfg(test)]
mod tests {
    use globibot_core::serenity::all::{ChannelId, MessageId};

    use super::*;

    fn delete(id: u64) -> Event {
        Event::MessageDelete {
            channel_id: ChannelId::new(1),
            message_id: MessageId::new(id),
        }
    }

    fn deleted_id(event: &Event) -> u64 {
        match event {
            Event::MessageDelete { message_id, .. } => message_id.get(),
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn plugins_catch_up_from_their_cursor() {
        let start = Instant::now();
        let deletes = HashSet::from([EventType::MessageDelete]);
        let mut log = Log::default();
        log.push(delete(1), start);
        log.push(delete(2), start);

        // First connection: everything kept
        let mut cursor = log.cursor("slap");
        let (seq, event) = log.next(&mut cursor, &deletes, start).unwrap();
        assert_eq!(deleted_id(&event), 1);
        log.cursors.insert("slap".into(), seq + 1);

        // Reconnecting resumes after the last event sent
        log.push(delete(3), start);
        let mut cursor = log.cursor("slap");
        let got = std::iter::from_fn(|| log.next(&mut cursor, &deletes, start))
            .map(|(_, event)| deleted_id(&event))
            .collect::<Vec<_>>();
        assert_eq!(got, [2, 3]);

        // Not subscribed to these
        let mut cursor = 0;
        assert!(
            log.next(
                &mut cursor,
                &HashSet::from([EventType::MessageCreate]),
                start
            )
            .is_none()
        );
        assert_eq!(cursor, 3);
    }

    #[test]
    fn old_events_are_dropped() {
        let start = Instant::now();
        let deletes = HashSet::from([EventType::MessageDelete]);
        let mut log = Log::default();
        log.push(delete(1), start);
        log.push(delete(2), start + KEPT_FOR / 2);

        let later = start + KEPT_FOR + Duration::from_secs(1);
        let mut cursor = 0;
        let (seq, event) = log.next(&mut cursor, &deletes, later).unwrap();
        assert_eq!((seq, deleted_id(&event)), (1, 2));

        for id in 1..=MAX_KEPT as u64 + 5 {
            log.push(delete(id), later);
        }
        assert_eq!(log.events.len(), MAX_KEPT);
        assert_eq!(log.end_seq(), 2 + MAX_KEPT as u64 + 5);
    }

    #[test]
    fn interactions_expire_quickly() {
        let interaction = EventType::InteractionCreate;
        assert!(!expired(interaction, Duration::from_secs(1)));
        assert!(expired(interaction, Duration::from_secs(3)));
        assert!(!expired(EventType::MessageCreate, Duration::from_secs(3)));
    }

    #[tokio::test]
    async fn late_and_returning_plugins_get_what_they_missed() {
        let publisher = Publisher::new();
        let (connections, incoming) = futures::channel::mpsc::unbounded();
        tokio::spawn(publisher.clone().run(incoming));
        let connect = || {
            let (bot_side, plugin_side) = tokio::io::duplex(4096);
            connections.unbounded_send(Ok(bot_side)).unwrap();
            let request = globibot_core::events::HandshakeRequest {
                id: "slap".into(),
                events: HashSet::from([EventType::MessageDelete]),
            };
            globibot_core::events::connect(plugin_side, request)
        };
        let next_id = async |plugin: &mut globibot_core::events::EventRead<_>| {
            let event = timeout(Duration::from_secs(1), plugin.next()).await;
            deleted_id(&event.unwrap().unwrap().unwrap())
        };

        // Before the plugin connects
        publisher.broadcast(delete(1));
        let mut plugin = connect().await.unwrap();
        assert_eq!(next_id(&mut plugin).await, 1);
        publisher.broadcast(delete(2));
        assert_eq!(next_id(&mut plugin).await, 2);

        // While it restarts
        drop(plugin);
        publisher.broadcast(delete(3));
        let mut plugin = connect().await.unwrap();
        assert_eq!(next_id(&mut plugin).await, 3);
        assert!(
            timeout(Duration::from_millis(100), plugin.next())
                .await
                .is_err()
        );
    }
}
