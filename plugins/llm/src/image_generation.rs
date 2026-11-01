//! The `generate_image` tool: an image model on OpenRouter makes the image, which is attached to
//! the answer. Avatars can be used as references, with their owner's consent.

use std::collections::HashMap;

use globibot_core::{
    rpc::{self, FileAttachment},
    serenity::all::UserId,
};
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::{
    LlmPlugin,
    openrouter::{ContentPart, ToolOutput},
    tool::{Tool, ToolCtx, llm_tool},
};

pub const DEFAULT_IMAGE_MODEL: &str = "google/gemini-3.1-flash-image";
const DEFAULT_PER_USER_PER_DAY: u32 = 10;
/// Each image travels to the bot as a JSON array of numbers, in a frame of at most 32MB
pub const MAX_PER_ANSWER: usize = 2;
const ASPECT_RATIOS: [&str; 7] = ["1:1", "3:2", "2:3", "4:3", "3:4", "16:9", "9:16"];

/// Saved with the other settings, changed on the website
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImageSettings {
    pub model: String,
    pub enabled: bool,
    /// The admin isn't limited
    pub per_user_per_day: u32,
}

impl ImageSettings {
    /// The model comes from `LLM_DEFAULT_IMAGE_MODEL_ID` if set
    pub fn defaults(default_model: Option<String>) -> Self {
        Self {
            model: default_model.unwrap_or_else(|| DEFAULT_IMAGE_MODEL.to_string()),
            enabled: true,
            per_user_per_day: DEFAULT_PER_USER_PER_DAY,
        }
    }
}

/// Images made per user today. ponytail: in memory, a restart resets the counts; save them with
/// the other state if that gets abused.
#[derive(Debug, Default)]
pub struct DailyCounts {
    by_user: HashMap<UserId, (u64, u32)>,
}

impl DailyCounts {
    /// How many `user_id` made on `day` (UTC day number)
    pub fn count(&self, user_id: UserId, day: u64) -> u32 {
        match self.by_user.get(&user_id) {
            Some(&(counted_day, count)) if counted_day == day => count,
            _ => 0,
        }
    }

    pub fn add(&mut self, user_id: UserId, day: u64) {
        let count = self.count(user_id, day);
        self.by_user.insert(user_id, (day, count + 1));
    }
}

llm_tool! {
    #[tool(name = "generate_image",
           description = format!(
               "Make an image from a description. It is attached to your answer: don't link it or say you can't send \
               images. People's avatars can be used as references (e.g. to draw someone as a pirate), only when \
               their owner agrees to share images with you, and so can the images shown to you with this \
               message. Each person can have a few images made per day. At most {MAX_PER_ANSWER} images per \
               answer."
           ))]
    pub struct GenerateImage {
        #[param(name = "prompt", description = "A detailed description of the image, in English")]
        prompt: String,
        #[param(name = "reference_user_ids", description = "Discord user ids whose avatars to use as references")]
        reference_user_ids: Option<Vec<String>>,
        #[param(name = "use_shown_images",
                description = "Use the images shown to you with this message (attached, replied to, or posted \
                    just before) as references, e.g. to edit a photo or draw the person in it")]
        use_shown_images: Option<bool>,
        #[param(name = "aspect_ratio", enum = ASPECT_RATIOS)]
        aspect_ratio: Option<String>,
    }
}

impl Tool for GenerateImage {
    async fn run(self, ctx: &ToolCtx<'_>) -> ToolOutput {
        ctx.plugin
            .generate_image(
                ctx.rpc,
                ctx.asked.author.id,
                ctx.files,
                ctx.shown_images,
                self,
            )
            .await
    }
}

impl LlmPlugin {
    /// Makes the image for `requester` and adds it to `files`, the answer's attachments
    pub async fn generate_image(
        &self,
        rpc: &rpc::ProtocolClient,
        requester: UserId,
        files: &Mutex<Vec<FileAttachment>>,
        shown_images: &[String],
        args: GenerateImage,
    ) -> ToolOutput {
        let settings = self.image_settings.read().clone();
        let day = crate::unix_now() / 86_400;
        if !settings.enabled {
            return ToolOutput::text("Image generation is turned off".to_string());
        }
        if files.lock().len() >= MAX_PER_ANSWER {
            return ToolOutput::text(format!("At most {MAX_PER_ANSWER} images per answer"));
        }
        let made_today = self.image_counts.lock().count(requester, day);
        if requester != self.admin_id && made_today >= settings.per_user_per_day {
            return ToolOutput::text(format!(
                "They already had {made_today} images made today, the limit is {}. Try again tomorrow (UTC).",
                settings.per_user_per_day
            ));
        }
        if args.prompt.trim().is_empty() {
            return ToolOutput::text("The prompt is empty".to_string());
        }
        let aspect_ratio = args
            .aspect_ratio
            .filter(|ratio| ASPECT_RATIOS.contains(&ratio.as_str()));

        // Already consented to when they were shown to the chat model
        let mut references = Vec::new();
        let mut text = String::new();
        if args.use_shown_images == Some(true) {
            if shown_images.is_empty() {
                text.push_str("No images were shown to you with this message\n");
            } else {
                references.extend_from_slice(shown_images);
                text.push_str(&format!(
                    "{} shown image(s) used as references\n",
                    shown_images.len()
                ));
            }
        }

        // The same consent rule as showing avatars to the chat model
        let reference_user_ids = args.reference_user_ids.unwrap_or_default();
        let avatars = self.avatars(rpc, requester, &reference_user_ids).await;
        references.extend(avatars.images.into_iter().filter_map(|image| match image {
            ContentPart::Image(image) => Some(image.image_url.url),
            ContentPart::Text(_) => None,
        }));
        if !avatars.text.is_empty() {
            // "attached as image n" means the avatar is a reference here
            text.push_str(&format!("Avatars:\n{}\n", avatars.text));
        }

        let generated = self
            .llm_client
            .generate_image(
                &settings.model,
                &args.prompt,
                aspect_ratio.as_deref(),
                references,
            )
            .await;
        match generated {
            Ok(image) => {
                self.image_counts.lock().add(requester, day);
                let mut files = files.lock();
                files.push(FileAttachment {
                    name: image.file_name(),
                    data: image.data,
                });
                text.push_str(&format!(
                    "Image generated, it will be attached to your answer (image {} of this answer)",
                    files.len()
                ));
            }
            Err(err) => {
                tracing::warn!("Image generation failed: {err:#}");
                text.push_str(&format!("The image generation failed: {err:#}"));
            }
        }

        ToolOutput::text(text)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn counts_reset_every_day() {
        let mut counts = DailyCounts::default();
        let alice = UserId::new(1);
        counts.add(alice, 10);
        counts.add(alice, 10);
        assert_eq!(counts.count(alice, 10), 2);
        assert_eq!(counts.count(UserId::new(2), 10), 0);
        assert_eq!(counts.count(alice, 11), 0);
        counts.add(alice, 11);
        assert_eq!(counts.count(alice, 11), 1);
    }

    #[test]
    fn arguments_parse_with_optional_fields() {
        let args: GenerateImage = serde_json::from_value(json!({ "prompt": "a cat" })).unwrap();
        assert_eq!(args.prompt, "a cat");
        assert!(args.reference_user_ids.is_none() && args.aspect_ratio.is_none());
    }

    #[test]
    fn settings_saved_without_images_get_defaults() {
        let defaults = ImageSettings::defaults(None);
        assert_eq!(defaults.model, DEFAULT_IMAGE_MODEL);
        assert!(defaults.enabled);
        assert_eq!(
            ImageSettings::defaults(Some("x/y".to_string())).model,
            "x/y"
        );
    }
}
