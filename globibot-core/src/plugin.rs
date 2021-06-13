use std::{borrow::Borrow, collections::HashSet, io, sync::Arc};

use futures::{Future, Stream, StreamExt};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::{
    events,
    events::{Event, EventRead, EventType},
    rpc,
    transport::Protocol,
};

pub trait Plugin {
    const ID: &'static str;

    type RpcPolicy: RpcContex;
    type EventsPolicy;
}

trait PluginID {
    const ID: &'static str;
}

impl<T: Plugin> PluginID for T {
    const ID: &'static str = T::ID;
}

pub struct HasRpc<const ENABLED: bool>;
pub struct HasEvents<const ENABLED: bool>;

pub trait RpcContex {
    type Context: Clone;
}

impl RpcContex for HasRpc<true> {
    type Context = rpc::ProtocolClient;
}

impl RpcContex for HasRpc<false> {
    type Context = ();
}

pub trait HandleEvents: Plugin {
    type Err;
    type Future: Future<Output = Result<(), Self::Err>>;

    fn on_event(
        self: Arc<Self>,
        ctx: <Self::RpcPolicy as RpcContex>::Context,
        event: Event,
    ) -> Self::Future;
}

pub trait PluginExt: Plugin {
    fn connect<R, E>(self, endpoints: Endpoints<R, E>) -> ConnectFut<Self, R, E>
    where
        Self: Sized,
        R: EndpointPolicy<Policy = Self::RpcPolicy>,
        E: EndpointPolicy<Policy = Self::EventsPolicy>,
    {
        async move {
            let rpc = endpoints.rpc.connect(Self::ID.to_owned()).await?;
            let events = endpoints.events.connect(Self::ID.to_owned()).await?;

            Ok(ConnectedPlugin {
                plugin: self,
                rpc,
                events,
            })
        }
    }
}

type ConnectFut<T, R: EndpointPolicy, E: EndpointPolicy> =
    impl Future<Output = io::Result<ConnectedPlugin<T, R::Client, E::Client>>>;

impl<T: Plugin> PluginExt for T {}

pub struct ConnectedPlugin<T, Rpc, Events> {
    plugin: T,
    rpc: Rpc,
    events: Events,
}

impl<T, Events> ConnectedPlugin<T, <T::RpcPolicy as RpcContex>::Context, Events>
where
    Events: Stream<Item = io::Result<events::Event>>,
    T: Plugin + HandleEvents,
    T::Err: std::fmt::Display,
{
    pub async fn handle_events(self) -> Result<(), io::Error> {
        let Self {
            plugin,
            rpc,
            events,
        } = self;
        let shared_plugin = Arc::new(plugin);

        events
            .for_each_concurrent(10, move |event_res| {
                let rpc = rpc.clone();
                let plugin = Arc::clone(&shared_plugin);

                async move {
                    match event_res {
                        Ok(event) => {
                            if let Err(why) = plugin.on_event(rpc, event).await {
                                tracing::warn!("Failed to handle event: {}", why.to_string());
                            }
                        }
                        Err(why) => {
                            tracing::error!("Invalid event: {}", why);
                        }
                    }
                }
            })
            .await;

        Ok(())
    }
}

pub struct Endpoints<Rpc, Events> {
    rpc: Rpc,
    events: Events,
}

impl Endpoints<UnboundRpc, UnboundEvents> {
    pub fn new() -> Self {
        Self {
            rpc: UnboundRpc,
            events: UnboundEvents,
        }
    }
}

impl Default for Endpoints<UnboundRpc, UnboundEvents> {
    fn default() -> Self {
        Self::new()
    }
}

impl<E> Endpoints<UnboundRpc, E> {
    pub fn rpc<P>(self, protocol: P) -> Endpoints<BoundRpc<P>, E> {
        Endpoints {
            rpc: BoundRpc(protocol),
            events: self.events,
        }
    }
}

impl<R> Endpoints<R, UnboundEvents> {
    pub fn events<P, E>(self, protocol: P, events: E) -> Endpoints<R, BoundEvents<P>>
    where
        E: IntoIterator,
        E::Item: Borrow<EventType>,
    {
        let events = events.into_iter().map(|e| *e.borrow()).collect();
        Endpoints {
            rpc: self.rpc,
            events: BoundEvents(protocol, events),
        }
    }
}

pub struct UnboundRpc;
pub struct UnboundEvents;
pub struct BoundRpc<P>(P);
pub struct BoundEvents<P>(P, HashSet<EventType>);

pub trait EndpointPolicy {
    type Policy;
    type Client;
    type ConnectFut: Future<Output = io::Result<Self::Client>>;

    fn connect(self, plugin_id: String) -> Self::ConnectFut;
}

impl<P> EndpointPolicy for BoundRpc<P>
where
    P: Protocol,
    P::Client: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Policy = HasRpc<true>;
    type Client = rpc::ProtocolClient;
    type ConnectFut = impl Future<Output = io::Result<Self::Client>>;

    fn connect(self, plugin_id: String) -> Self::ConnectFut {
        async move {
            let transport = self.0.connect().await?;
            let handshake_request = rpc::HandshakeRequest { id: plugin_id };
            let (client, dispatch) =
                rpc::connect(Default::default(), handshake_request, transport).await?;
            tokio::spawn(dispatch);
            Ok(client)
        }
    }
}

impl<P> EndpointPolicy for BoundEvents<P>
where
    P: Protocol,
    P::Client: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    type Policy = HasEvents<true>;
    type Client = EventRead<P::Client>;
    type ConnectFut = impl Future<Output = io::Result<Self::Client>>;

    fn connect(self, plugin_id: String) -> Self::ConnectFut {
        async move {
            let transport = self.0.connect().await?;
            let handshake_request = events::HandshakeRequest {
                id: plugin_id,
                events: self.1,
            };
            let events = events::connect(transport, handshake_request).await?;
            Ok(events)
        }
    }
}

impl EndpointPolicy for UnboundRpc {
    type Policy = HasRpc<false>;
    type Client = ();
    type ConnectFut = impl Future<Output = io::Result<Self::Client>>;

    fn connect(self, _plugin_id: String) -> Self::ConnectFut {
        async { Ok(()) }
    }
}

impl EndpointPolicy for UnboundEvents {
    type Policy = HasEvents<false>;
    type Client = ();
    type ConnectFut = impl Future<Output = io::Result<Self::Client>>;

    fn connect(self, _plugin_id: String) -> Self::ConnectFut {
        async { Ok(()) }
    }
}
