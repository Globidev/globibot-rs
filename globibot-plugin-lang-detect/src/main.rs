#![feature(type_alias_impl_trait, let_else)]

use futures::Future;
use serenity::model::id::ChannelId;
use std::sync::Arc;

use globibot_core::{
    events::{Event, EventType},
    plugin::{Endpoints, HandleEvents, HasEvents, HasRpc, Plugin, PluginExt},
    rpc::{self, context::current as rpc_context},
    serenity::{self, model::channel::ReactionType},
    transport::Tcp,
};
use globibot_plugin_lang_detect::{flag_from_code, LanguageDetector};

struct LangDetectPlugin {
    enabled_channels: Vec<ChannelId>,
    detector: LanguageDetector,
}

impl Plugin for LangDetectPlugin {
    const ID: &'static str = "LangDetect";

    type RpcPolicy = HasRpc<true>;
    type EventsPolicy = HasEvents<true>;
}

fn load_env(key: &str) -> String {
    std::env::var(key)
        .unwrap_or_else(|why| panic!("Failed to load environment variable '{}': {}", key, why))
}

#[tokio::main]
async fn main() {
    let enabled_channels = load_env("LANG_DETECT_ENABLED_CHANNELS")
        .split(',')
        .map(|raw| raw.parse().expect("Badly formed channel ID"))
        .collect();

    let plugin = LangDetectPlugin {
        enabled_channels,
        detector: LanguageDetector::new(load_env("LANG_DETECT_API_KEY")),
    };

    let events = [EventType::MessageCreate];

    let subscriber_addr = load_env("SUBSCRIBER_ADDR");
    let rpc_addr = load_env("RPC_ADDR");

    let endpoints = Endpoints::new()
        .rpc(Tcp::new(rpc_addr))
        .events(Tcp::new(subscriber_addr), &events);

    plugin
        .connect(endpoints)
        .await
        .expect("Failed to connect plugin")
        .handle_events()
        .await
        .expect("Failed to run plugin");
}

impl HandleEvents for LangDetectPlugin {
    type Err = anyhow::Error;
    type Future = impl Future<Output = Result<(), Self::Err>>;

    fn on_event(self: Arc<Self>, rpc: rpc::ProtocolClient, event: Event) -> Self::Future {
        async move {
            let Event::MessageCreate { message } = event else { return Ok(()) };

            if !self.enabled_channels.contains(&message.channel_id) {
                return Ok(());
            }

            let channel_id = message.channel_id;
            let message_id = message.id;

            let content_safe = rpc
                .content_safe(rpc_context(), message.content, message.guild_id)
                .await??;

            // Don't detect single emoji messages
            if serenity::utils::parse_emoji(&content_safe).is_some() {
                return Ok(());
            }

            let detection = self.detector.detect_language(&content_safe).await?;

            if detection.is_reliable && detection.language != "en" {
                if let Some(flag) = flag_from_code(&detection.language) {
                    let reaction = ReactionType::Unicode(flag.to_owned());
                    rpc.create_reaction(rpc_context(), channel_id, message_id, reaction)
                        .await??;
                }
            }

            Ok(())
        }
    }
}
