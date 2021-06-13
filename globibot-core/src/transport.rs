use std::{io, path::Path};

use futures::{
    future::{self, Future},
    Stream, TryFutureExt,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpListener, TcpStream, ToSocketAddrs, UnixListener, UnixStream},
};
use tokio_serde::{formats::Json, Framed as SerdeFramed};
use tokio_stream::wrappers::{TcpListenerStream, UnixListenerStream};
use tokio_util::codec::{Framed, LengthDelimitedCodec};

#[derive(Serialize, Deserialize)]
pub enum NoData {}

pub type FramedStream<T, Req, Resp> =
    SerdeFramed<Framed<T, LengthDelimitedCodec>, Req, Resp, Json<Req, Resp>>;
pub type FramedRead<T, Req> = FramedStream<T, Req, NoData>;
pub type FramedWrite<T, Resp> = FramedStream<T, NoData, Resp>;

pub trait Protocol {
    type Client;
    type ClientStream: Stream<Item = io::Result<Self::Client>>;
    type ListenFuture: Future<Output = io::Result<Self::ClientStream>>;
    type ConnectFuture: Future<Output = io::Result<Self::Client>>;

    fn listen(self) -> Self::ListenFuture;
    fn connect(self) -> Self::ConnectFuture;
}

pub(crate) fn frame_transport<T, Req, Resp>(transport: T) -> FramedStream<T, Req, Resp>
where
    T: AsyncRead + AsyncWrite,
    Req: Serialize,
    Resp: DeserializeOwned,
{
    let mut codec = LengthDelimitedCodec::new();
    codec.set_max_frame_length(32 * 1024 * 1024);
    let length_framed_transport = Framed::new(transport, codec);
    SerdeFramed::new(length_framed_transport, Json::default())
}

pub struct Ipc<P> {
    path: P,
}

impl<P: AsRef<Path>> Ipc<P> {
    pub fn new(path: P) -> Self {
        Self { path }
    }
}

pub struct Tcp<A> {
    addr: A,
}

impl<A: ToSocketAddrs> Tcp<A> {
    pub fn new(addr: A) -> Self {
        Self { addr }
    }
}

impl<P> Protocol for Ipc<P>
where
    P: AsRef<Path>,
{
    type Client = UnixStream;
    type ClientStream = UnixListenerStream;
    type ListenFuture = impl Future<Output = io::Result<Self::ClientStream>>;
    type ConnectFuture = impl Future<Output = io::Result<Self::Client>>;

    fn listen(self) -> Self::ListenFuture {
        let _ = std::fs::remove_file(&self.path);
        future::ready(UnixListener::bind(self.path).map(UnixListenerStream::new))
    }

    fn connect(self) -> Self::ConnectFuture {
        UnixStream::connect(self.path)
    }
}

impl<A> Protocol for Tcp<A>
where
    A: ToSocketAddrs,
{
    type Client = TcpStream;
    type ClientStream = TcpListenerStream;
    type ListenFuture = impl Future<Output = io::Result<Self::ClientStream>>;
    type ConnectFuture = impl Future<Output = io::Result<Self::Client>>;

    fn listen(self) -> Self::ListenFuture {
        TcpListener::bind(self.addr).map_ok(TcpListenerStream::new)
    }

    fn connect(self) -> Self::ConnectFuture {
        TcpStream::connect(self.addr)
    }
}
