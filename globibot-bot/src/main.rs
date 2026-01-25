#![feature(trait_alias)]

mod discord;
mod events;
mod rpc;
mod web;

use std::env;

use futures::TryFutureExt;
use globibot_core::transport::{Protocol, Tcp};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt::init();

    let subscriber_addr = env::var("SUBSCRIBER_ADDR")?;
    let rpc_addr = env::var("RPC_ADDR")?;
    let web_addr = env::var("WEB_ADDR")?;
    let discord_token = env::var("DISCORD_TOKEN")?;
    let application_id = env::var("APPLICATION_ID")?.parse()?;
    let application_secret = env::var("APPLICATION_SECRET")?;
    let discord_oauth_authorize_link = env::var("DISCORD_OAUTH_AUTHORIZE_LINK")?;
    let cookie_secret = env::var("COOKIE_SECRET")?;
    let discord_oauth_guild_id = env::var("DISCORD_OAUTH_GUILD_ID")?.parse()?;

    let ev_publisher = events::Publisher::new();
    let raw_ev_subscribers = Tcp::new(subscriber_addr).listen().await?;
    let raw_rpc_clients = Tcp::new(rpc_addr).listen().await?;

    let mut discord_client =
        discord::client(&discord_token, application_id, ev_publisher.clone()).await?;

    let publish_events = ev_publisher.run(raw_ev_subscribers);
    let run_rpc_server = rpc::run_server(
        raw_rpc_clients,
        discord_client.cache.clone(),
        discord_client.http.clone(),
    );
    let run_discord_client = discord_client.start();
    let storage = globibot_core::storage::RedisStorage::from_env().await?;
    let web_server = web::WebServer::new(
        storage,
        &cookie_secret,
        application_id,
        application_secret,
        discord_oauth_authorize_link,
        discord_oauth_guild_id,
    );
    let run_web_server = web_server.serve(web_addr);

    tracing::info!("Starting bot...");

    futures::try_join!(
        publish_events.err_into::<anyhow::Error>(),
        run_rpc_server.err_into(),
        run_discord_client.err_into(),
        run_web_server.err_into(),
    )?;

    Ok(())
}
