use std::{io, path::Path, time::Duration};

use futures::{Future, Stream, StreamExt};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    net::{TcpListener, TcpStream, ToSocketAddrs, UnixListener, UnixStream},
    time::timeout,
};
use tokio_serde::{Framed as SerdeFramed, formats::Json};
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

    fn listen(
        self,
    ) -> impl Future<Output = io::Result<impl Stream<Item = io::Result<Self::Client>>>>;
    fn connect(self) -> impl Future<Output = io::Result<Self::Client>>;
}

pub(crate) fn frame_transport<T, Req, Resp>(transport: T) -> FramedStream<T, Req, Resp>
where
    T: AsyncRead + AsyncWrite,
    Req: Serialize,
    Resp: DeserializeOwned,
{
    SerdeFramed::new(length_framed(transport), Json::default())
}

fn length_framed<T: AsyncRead + AsyncWrite>(transport: T) -> Framed<T, LengthDelimitedCodec> {
    let mut codec = LengthDelimitedCodec::new();
    codec.set_max_frame_length(32 * 1024 * 1024);
    Framed::new(transport, codec)
}

#[derive(Debug, thiserror::Error)]
pub enum AcceptError {
    #[error("IO error: {0}")]
    IO(#[from] io::Error),

    #[error("Handshake timed out")]
    HandshakeTimedOut,

    #[error("Handshake missing")]
    HandshakeMissing,
}

/// Reads a client's handshake, the first frame, then frames the rest of the stream with the same
/// buffer. Framing it again would lose what was read past the handshake, such as an RPC client's
/// first request when it arrives with it.
pub(crate) async fn accept_handshake<T, H, Req, Resp>(
    transport: T,
) -> Result<(H, FramedStream<T, Req, Resp>), AcceptError>
where
    T: AsyncRead + AsyncWrite + Unpin,
    H: DeserializeOwned,
{
    let mut frames = length_framed(transport);
    let frame = timeout(Duration::from_secs(5), frames.next())
        .await
        .map_err(|_timed_out| AcceptError::HandshakeTimedOut)?
        .ok_or(AcceptError::HandshakeMissing)??;
    let handshake = serde_json::from_slice(&frame).map_err(io::Error::from)?;
    Ok((handshake, SerdeFramed::new(frames, Json::default())))
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

    async fn listen(self) -> io::Result<impl Stream<Item = io::Result<Self::Client>>> {
        let _ = std::fs::remove_file(&self.path);
        let listener = UnixListener::bind(self.path)?;
        Ok(UnixListenerStream::new(listener))
    }

    async fn connect(self) -> io::Result<Self::Client> {
        UnixStream::connect(self.path).await
    }
}

impl<A> Protocol for Tcp<A>
where
    A: ToSocketAddrs,
{
    type Client = TcpStream;

    async fn listen(self) -> io::Result<impl Stream<Item = io::Result<Self::Client>>> {
        let listener = TcpListener::bind(self.addr).await?;
        Ok(TcpListenerStream::new(listener))
    }

    async fn connect(self) -> io::Result<Self::Client> {
        TcpStream::connect(self.addr).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::SinkExt;

    #[tokio::test]
    async fn keeps_what_arrives_with_the_handshake() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let mut client = length_framed(
            TcpStream::connect(listener.local_addr().unwrap())
                .await
                .unwrap(),
        );
        let (server, _) = listener.accept().await.unwrap();

        // Both frames are buffered before the server reads anything
        client.send(r#""hello""#.into()).await.unwrap();
        client.send(r#""first request""#.into()).await.unwrap();

        let (handshake, mut rest) = accept_handshake::<_, String, String, NoData>(server)
            .await
            .unwrap();
        assert_eq!(handshake, "hello");
        assert_eq!(rest.next().await.unwrap().unwrap(), "first request");
    }
}
