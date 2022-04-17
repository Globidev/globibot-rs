#![feature(type_alias_impl_trait, trait_alias, associated_type_bounds)]

mod discord;
mod events;
mod rpc;

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
    let dicord_cache_and_http = discord_client.cache_and_http.clone();

    let publish_events = events::run_publisher(raw_event_subscribers, publisher);
    let run_rpc_server = rpc::run_server(raw_rpc_clients, dicord_cache_and_http);
    let run_discord_client = discord_client.start();

    tracing::info!("Bot running");

    futures::try_join!(
        publish_events.err_into::<AppError>(),
        run_rpc_server.err_into(),
        run_discord_client.err_into(),
    )?;

    Ok(())
}
#[derive(Debug, derive_more::From)]
enum AppError {
    IO(io::Error),
    Discord(globibot_core::serenity::Error),
    MissingEnvVar(env::VarError),
    MalformedApplicationId(ParseIntError),
}
