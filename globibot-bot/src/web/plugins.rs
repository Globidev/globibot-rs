use axum::{
    BoxError, Json,
    extract::State,
    response::{IntoResponse, sse},
};
use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};
use tokio::sync::broadcast;

pub fn router() -> axum::Router<()> {
    use axum::routing::get;

    let sse_receiver = SseReceiver {
        rx: REGISTRY.lock().unwrap().tx.subscribe(),
    };

    axum::Router::new()
        .route("/", get(list_plugins))
        .route("/sse", get(stream_events).with_state(sse_receiver))
}

async fn list_plugins() -> Json<Vec<ConnectedPlugin>> {
    let plugins = REGISTRY.lock().unwrap().by_id.values().cloned().collect();

    Json(plugins)
}

async fn stream_events(State(SseReceiver { rx }): State<SseReceiver>) -> impl IntoResponse {
    use futures::TryStreamExt;

    let event_stream = tokio_stream::wrappers::BroadcastStream::new(rx)
        .err_into::<BoxError>()
        .and_then(|msg| async { Ok(sse::Event::default().json_data(msg)?) });

    sse::Sse::new(event_stream)
}

struct SseReceiver {
    rx: broadcast::Receiver<RegistryEvent>,
}

impl Clone for SseReceiver {
    fn clone(&self) -> Self {
        Self {
            rx: self.rx.resubscribe(),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize)]
enum RegistryEvent {
    UpsertedPlugin(ConnectedPlugin),
    RemovedPlugin(String),
}

#[derive(Debug, Clone, serde::Serialize)]
struct ConnectedPlugin {
    name: String,
    has_rpc: bool,
    has_events: bool,
    has_web_api: bool,
    startup_ts: u64,
}

pub static REGISTRY: LazyLock<Mutex<PluginRegistry>> = LazyLock::new(|| {
    Mutex::new(PluginRegistry {
        by_id: HashMap::new(),
        tx: broadcast::channel(1 << 8).0,
    })
});

#[derive(Debug)]
pub struct PluginRegistry {
    by_id: HashMap<String, ConnectedPlugin>,
    tx: broadcast::Sender<RegistryEvent>,
}

impl PluginRegistry {
    pub fn register_rpc(&mut self, name: &str) {
        let plugin = self.get_or_create(name);
        plugin.has_rpc = true;

        let plugin = plugin.clone();
        self.tx.send(RegistryEvent::UpsertedPlugin(plugin)).ok();
    }

    pub fn register_events(&mut self, name: &str) {
        let existing_plugin = self.get_or_create(name);
        existing_plugin.has_events = true;

        let plugin = existing_plugin.clone();
        self.tx.send(RegistryEvent::UpsertedPlugin(plugin)).ok();
    }

    pub fn register_web_api(&mut self, name: &str) {
        let existing_plugin = self.get_or_create(name);
        existing_plugin.has_web_api = true;

        let plugin = existing_plugin.clone();
        self.tx.send(RegistryEvent::UpsertedPlugin(plugin)).ok();
    }

    pub fn unregister_rpc(&mut self, name: &str) {
        let Some(_plugin) = self.by_id.remove(name) else {
            return;
        };
        let message = RegistryEvent::RemovedPlugin(name.to_string());
        self.tx.send(message).ok();
    }

    fn get_or_create(&mut self, name: &str) -> &mut ConnectedPlugin {
        self.by_id
            .entry(name.to_string())
            .or_insert_with(|| ConnectedPlugin {
                name: name.to_string(),
                has_rpc: false,
                has_events: false,
                has_web_api: false,
                startup_ts: std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
            })
    }
}
