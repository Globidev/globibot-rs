#![feature(trait_alias)]

mod discord;
mod events;
mod rpc;
mod web;

use std::{env, io, num::ParseIntError};

use futures::TryFutureExt;
use globibot_core::transport::{Protocol, Tcp};

#[tokio::main]
async fn main() -> Result<(), AppError> {
    tracing_subscriber::fmt::init();

    let publisher = events::Publisher::new();

    let subscriber_addr = env::var("SUBSCRIBER_ADDR")?;
    let rpc_addr = env::var("RPC_ADDR")?;

    let raw_event_subscribers = Tcp::new(subscriber_addr).listen().await?;
    let raw_rpc_clients = Tcp::new(rpc_addr).listen().await?;

    let discord_token = env::var("DISCORD_TOKEN")?;
    let application_id = env::var("APPLICATION_ID")?.parse()?;
    let mut discord_client =
        discord::client(&discord_token, publisher.clone(), application_id).await?;

    let publish_events = events::run_publisher(raw_event_subscribers, publisher);
    let run_rpc_server = rpc::run_server(
        raw_rpc_clients,
        discord_client.cache.clone(),
        discord_client.http.clone(),
    );
    let run_discord_client = discord_client.start();
    let run_web_server = web::run_server();

    tracing::info!("Starting bot...");

    futures::try_join!(
        publish_events.err_into::<AppError>(),
        run_rpc_server.err_into(),
        run_discord_client.err_into(),
        run_web_server.err_into(),
    )?;

    Ok(())
}
#[derive(Debug, thiserror::Error)]
enum AppError {
    #[error("IO error: {0}")]
    IO(#[from] io::Error),

    #[error("Discord error: {0}")]
    Discord(Box<globibot_core::serenity::Error>),

    #[error("Missing environment variable: {0}")]
    MissingEnvVar(#[from] env::VarError),

    #[error("Malformed application ID: {0}")]
    MalformedApplicationId(#[from] ParseIntError),
}

impl From<globibot_core::serenity::Error> for AppError {
    fn from(err: globibot_core::serenity::Error) -> Self {
        AppError::Discord(Box::new(err))
    }
}
