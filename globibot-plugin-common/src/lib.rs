pub mod imageops;

pub use anyhow;
pub use gif;
pub use image;

pub fn load_env(key: &str) -> String {
    std::env::var(key)
        .unwrap_or_else(|why| panic!("Failed to load environment variable '{key}': {why}"))
}

pub mod endpoints {
    use anyhow::Context;
    use globibot_core::{
        events::EventType,
        plugin::{BoundEvents, BoundRpc, Endpoints},
        transport::Tcp,
    };

    type TcpEndpoints = Endpoints<BoundRpc<Tcp<String>>, BoundEvents<Tcp<String>>>;

    pub fn tcp_from_env(
        events: impl IntoIterator<Item = EventType>,
    ) -> anyhow::Result<TcpEndpoints> {
        let subscriber_addr = std::env::var("SUBSCRIBER_ADDR")
            .context("Missing 'SUBSCRIBER_ADDR' environment variable")?;
        let rpc_addr =
            std::env::var("RPC_ADDR").context("Missing 'RPC_ADDR' environment variable")?;

        Ok(Endpoints::new()
            .rpc(Tcp::new(rpc_addr))
            .events(Tcp::new(subscriber_addr), events))
    }
}
