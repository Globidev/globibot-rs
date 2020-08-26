#![feature(bool_to_option, type_alias_impl_trait, trait_alias)]

mod discord;
mod events;
mod rpc;

use futures::TryFutureExt;
use std::{env, io};

use tracing::{info};

#[tokio::main]
async fn main() -> Result<(), AppError> {
    tracing_subscriber::fmt::init();

    let shared_publisher = events::SharedPublisher::default();

    let mut events_endpoint = ipc_endpoint("globibot-events");
    let raw_event_subscribers = events_endpoint.incoming()?;

    let mut rpc_endpoint = ipc_endpoint("globibot-rpc");
    let raw_rpc_clients = rpc_endpoint.incoming()?;

    let discord_token = env::var("DISCORD_TOKEN")?;
    let mut discord_client = discord::client(&discord_token, shared_publisher.clone()).await?;
    let dicord_http = discord_client.cache_and_http.http.clone();

    let publish_events = events::run_publisher(raw_event_subscribers, shared_publisher);
    let run_rpc_server = rpc::run_server(raw_rpc_clients, dicord_http);
    let run_discord_client = discord_client.start();

    info!("Bot running");

    futures::try_join!(
        publish_events.err_into(),
        run_rpc_server.err_into(),
        run_discord_client,
    )?;

    Ok(())
}

fn ipc_endpoint(path: impl Into<String>) -> parity_tokio_ipc::Endpoint {
    let path = path.into();
    let _ = std::fs::remove_file(&path);
    parity_tokio_ipc::Endpoint::new(path)
}

#[derive(Debug, derive_more::From)]
enum AppError {
    IO(io::Error),
    Discord(serenity::Error),
    MissingToken(env::VarError),
}
