use std::sync::atomic::Ordering;

use globibot_core::storage::{self, StorageValue};

use crate::LlmPlugin;

/// Settings changed at runtime (slash commands or the website), saved so they survive restarts
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LlmSettings {
    pub model: String,
    pub personality: String,
    pub context_window_size: usize,
}

impl StorageValue for LlmSettings {
    type Format = storage::JsonFormat;

    const REDIS_NS: &'static str = "llm_settings";
}

pub const SETTINGS_KEY: &str = "latest";

impl LlmPlugin {
    /// Saves the current settings. Failures are logged rather than returned: the change still
    /// applies until the next restart
    pub async fn save_settings(&self) {
        let settings = LlmSettings {
            model: self.llm_model.read().clone(),
            personality: self.personality.read().name.clone(),
            context_window_size: self.context_window_size.load(Ordering::Relaxed),
        };

        if let Err(err) = self.storage.clone().set(SETTINGS_KEY, &settings).await {
            tracing::error!("Failed to save LLM settings: {err:?}");
        }
    }
}
