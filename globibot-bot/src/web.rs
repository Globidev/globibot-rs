use std::{
    collections::HashMap,
    sync::{LazyLock, Mutex},
};

use axum::{
    BoxError, Json, Router,
    extract::State,
    response::{Sse, sse::Event},
    routing::get,
};
use futures::{Stream, TryStreamExt};
use tokio::sync::broadcast::Receiver;

pub async fn run_server() -> std::io::Result<()> {
    let app = Router::new() //
        .route("/", get(async || "Globibot Web Server"))
        .route("/plugins", get(list_plugins))
        .route("/sse", get(stream_events))
        .with_state(SseMessageReceiver {
            rx: WEB_STATE.lock().unwrap().tx.subscribe(),
        });

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8001").await?;
    axum::serve(listener, app).await?;

    Ok(())
}

async fn list_plugins() -> Json<Vec<ConnectedPlugin>> {
    let plugins = WEB_STATE
        .lock()
        .unwrap()
        .plugins
        .values()
        .cloned()
        .collect();
    Json(plugins)
}

#[axum::debug_handler]
async fn stream_events(
    State(SseMessageReceiver { rx }): State<SseMessageReceiver>,
) -> Sse<impl Stream<Item = Result<Event, BoxError>>> {
    use tokio_stream::wrappers::BroadcastStream;

    let event_stream = BroadcastStream::new(rx)
        .err_into()
        .and_then(|msg| async { Ok(Event::default().json_data(msg)?) });
    Sse::new(event_stream)
}

#[derive(Debug, Clone, serde::Serialize)]
enum SseMessage {
    UpsertedPlugin(ConnectedPlugin),
    RemovedPlugin(String),
}

#[derive(Debug, Clone, serde::Serialize)]
struct ConnectedPlugin {
    name: String,
    has_rpc: bool,
    has_events: bool,
}

struct SseMessageReceiver {
    rx: Receiver<SseMessage>,
}

impl Clone for SseMessageReceiver {
    fn clone(&self) -> Self {
        Self {
            rx: self.rx.resubscribe(),
        }
    }
}

pub static WEB_STATE: LazyLock<Mutex<WebServerState>> = LazyLock::new(|| {
    Mutex::new(WebServerState {
        plugins: HashMap::new(),
        tx: tokio::sync::broadcast::channel(1 << 8).0,
    })
});

#[derive(Debug)]
pub struct WebServerState {
    plugins: HashMap<String, ConnectedPlugin>,
    tx: tokio::sync::broadcast::Sender<SseMessage>,
}

impl WebServerState {
    pub fn register_plugin_rpc(&mut self, name: &str) {
        let plugin = self.get_or_create_plugin(name);
        plugin.has_rpc = true;

        let plugin = plugin.clone();
        self.tx.send(SseMessage::UpsertedPlugin(plugin)).ok();
    }

    pub fn register_plugin_events(&mut self, name: &str) {
        let existing_plugin = self.get_or_create_plugin(name);
        existing_plugin.has_events = true;

        let plugin = existing_plugin.clone();
        self.tx.send(SseMessage::UpsertedPlugin(plugin)).ok();
    }

    fn get_or_create_plugin(&mut self, name: &str) -> &mut ConnectedPlugin {
        self.plugins
            .entry(name.to_string())
            .or_insert_with(|| ConnectedPlugin {
                name: name.to_string(),
                has_rpc: false,
                has_events: false,
            })
    }

    pub fn remove_plugin(&mut self, name: &str) {
        self.plugins.remove(name);
        self.tx
            .send(SseMessage::RemovedPlugin(name.to_string()))
            .ok();
    }
}
