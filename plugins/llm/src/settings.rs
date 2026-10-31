use std::sync::atomic::Ordering;

use globibot_core::storage::{self, StorageValue};

use crate::{LlmPlugin, image_generation::ImageSettings};

/// Settings changed at runtime (slash commands or the website), saved so they survive restarts
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct LlmSettings {
    pub model: String,
    pub personality: String,
    pub context_window_size: usize,
    /// `None` in settings saved before image generation existed: the defaults apply
    #[serde(default)]
    pub images: Option<ImageSettings>,
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
            images: Some(self.image_settings.read().clone()),
        };

        if let Err(err) = self.storage.clone().set(SETTINGS_KEY, &settings).await {
            tracing::error!("Failed to save LLM settings: {err:?}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_saved_before_images_still_load() {
        let json = serde_json::json!({
            "model": "m", "personality": "p", "context_window_size": 10
        });
        let settings = serde_json::from_value::<LlmSettings>(json).unwrap();
        assert!(settings.images.is_none());
    }
}
