use std::{io, sync::Arc};

use futures::{pin_mut, Stream, StreamExt};
use globibot_core::rpc::{self, AcceptError};
use serenity::{
    http::Http as DiscordHttp,
    model::{
        channel::Message,
        id::{ChannelId, MessageId},
    },
};
use tarpc::{context::Context, server::Channel};
use tokio::io::{AsyncRead, AsyncWrite};

use rpc::{Protocol, ProtocolResult, ServerChannel};
use tracing::{debug, info, warn};

pub async fn run_server<S, T>(transports: S, http: Arc<DiscordHttp>) -> io::Result<()>
where
    S: Stream<Item = io::Result<T>>,
    T: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    pin_mut!(transports);

    while let Some(transport_result) = transports.next().await {
        let transport = transport_result?;
        match rpc::accept(Default::default(), transport).await {
            Ok((request, client)) => {
                let handle_client = respond_to_rpc_client(client, Arc::clone(&http));
                tokio::spawn(handle_client);
                info!("New RPC client spawned: '{}'", request.id);
            }
            Err(AcceptError::IO(err)) => {
                warn!("IO error while accepting new RPC client : {}", err);
            }
            Err(AcceptError::HandshakeMissing) => warn!("RPC client did not send a handshake"),
            Err(AcceptError::HandshakeTimedOut) => {
                warn!("RPC client did not send a handshake in time")
            }
        }
    }

    Ok(())
}

async fn respond_to_rpc_client<Transport>(
    client: ServerChannel<Transport>,
    http: Arc<DiscordHttp>,
) -> io::Result<()>
where
    Transport: AsyncRead + AsyncWrite,
{
    let server = Server { discord_http: http };
    let handle_requests = client.respond_with(server.serve());
    pin_mut!(handle_requests);

    while let Some(handler_result) = handle_requests.next().await {
        debug!("Handling RPC request");
        let handler = handler_result?;
        handler.await;
    }

    info!("Ended connection with RPC client");

    Ok(())
}

#[derive(Clone)]
struct Server {
    discord_http: Arc<DiscordHttp>,
}

#[tarpc::server]
impl Protocol for Server {
    async fn send_message(
        self,
        _ctx: Context,
        chan_id: ChannelId,
        content: String,
    ) -> ProtocolResult<Message> {
        Ok(chan_id
            .send_message(self.discord_http, |message| {
                message.content(content);
                message
            })
            .await?)
    }

    async fn delete_message(
        self,
        _ctx: Context,
        chan_id: ChannelId,
        message_id: MessageId,
    ) -> ProtocolResult<()> {
        Ok(chan_id
            .delete_message(self.discord_http, message_id)
            .await?)
    }
}
