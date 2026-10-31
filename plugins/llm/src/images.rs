//! Images in chat messages.
//!
//! Every image posted gets a short text description (a caption) that goes into the channel's
//! context, so the context stays text: cheap, cache-friendly, and unaffected by Discord's image
//! links expiring. The images themselves are only sent when someone pings the bot about them.
//!
//! Images only go to the model provider with their author's consent: they have the images role,
//! or they shared them with the bot directly (a ping or a DM). Other images are only marked as
//! posted.
//!
//! Images are always downloaded and sent inline: the model providers can't fetch Discord's CDN
//! (Google gets a 403). They go through Discord's media proxy, resized to fit 1024px and
//! converted to a static WebP (GIFs become their first frame).

use base64::Engine;
use globibot_core::serenity::all::{Attachment, Message as DiscordMessage, UserId};
use serde::{Deserialize, Serialize};

use crate::{
    openrouter::{self, ContentPart, ImageContentPart, ImageUrl, Message, Role, TextContentPart},
    usage::UsageKind,
};

/// Images taken from one message, the rest are ignored
pub const MAX_IMAGES_PER_MESSAGE: usize = 4;

const MAX_IMAGE_SIDE: u32 = 1024;
const MAX_DOWNLOAD_BYTES: usize = 8 * 1024 * 1024;
const MAX_CAPTION_CHARS: usize = 1_500;

/// Stands for images the bot can't see
pub const NOT_SHARED: &str = "[Image not shared with the bot]";

/// Why the images of a message may be sent to the model provider
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Consent {
    /// The author shared them with the bot, in a ping or a DM: covers that message for good
    Shared,
    /// The author has the images role: lasts as long as they keep it
    Role(UserId),
}

pub fn consent(in_dm: bool, shared: bool, author: UserId, has_role: bool) -> Option<Consent> {
    if in_dm || shared {
        Some(Consent::Shared)
    } else if has_role {
        Some(Consent::Role(author))
    } else {
        None
    }
}

/// Whether a text part is an image caption, as put in the context once ready
pub fn is_caption(text: &str) -> bool {
    text.starts_with("[Image: ") || text.starts_with("[Images: ")
}

/// The images of a message that can be shown to the model
#[derive(Debug, Default)]
pub struct MessageImages {
    /// Resized WebP links through Discord's media proxy. They expire after about a day.
    pub urls: Vec<String>,
    pub spoilers: usize,
}

impl MessageImages {
    pub fn of(message: &DiscordMessage) -> Self {
        let mut images = Self::default();
        for attachment in message.attachments.iter().filter(|a| is_image(a)) {
            if attachment.filename.starts_with("SPOILER_") {
                images.spoilers += 1;
            } else if images.urls.len() < MAX_IMAGES_PER_MESSAGE
                && let Some(url) = proxied_url(attachment)
            {
                images.urls.push(url);
            }
        }
        images
    }

    /// Stands in the context until the caption replaces it
    pub fn placeholder(&self) -> &'static str {
        if self.urls.len() > 1 {
            "[Images]"
        } else {
            "[Image]"
        }
    }
}

fn is_image(attachment: &Attachment) -> bool {
    attachment
        .content_type
        .as_deref()
        .is_some_and(|content_type| content_type.starts_with("image/"))
        && attachment.dimensions().is_some()
}

fn proxied_url(attachment: &Attachment) -> Option<String> {
    let (width, height) = attachment.dimensions()?;
    let (width, height) = fit(width, height, MAX_IMAGE_SIDE);
    let mut url = reqwest::Url::parse(&attachment.proxy_url).ok()?;
    url.query_pairs_mut()
        .append_pair("width", &width.to_string())
        .append_pair("height", &height.to_string())
        .append_pair("format", "webp");
    Some(url.into())
}

/// Scales down to fit in a `max` square, keeping the aspect ratio. Never scales up.
fn fit(width: u32, height: u32, max: u32) -> (u32, u32) {
    let longest = width.max(height);
    if longest <= max {
        return (width, height);
    }
    let scale = |side: u32| ((side as u64 * max as u64) / longest as u64).max(1) as u32;
    (scale(width), scale(height))
}

/// What Tenor and Giphy links show, read from their URL: `tenor.com/view/cat-dancing-gif-123`
/// is "cat dancing". Discord's GIF picker posts Tenor links, so that's most GIFs, for free.
pub fn gif_descriptions(content: &str) -> Vec<String> {
    content
        .split_whitespace()
        .filter_map(|word| reqwest::Url::parse(word.trim_matches(['<', '>'])).ok())
        .filter_map(|url| {
            let host = url.host_str()?.trim_start_matches("www.");
            let segments = url.path_segments()?.collect::<Vec<_>>();
            let slug = match host {
                // tenor.com/view/<slug>-gif-<id>, sometimes with a language first
                "tenor.com" => {
                    let view = segments.iter().position(|s| *s == "view")?;
                    let slug = segments.get(view + 1)?;
                    slug.rsplit_once("-gif-").map_or(*slug, |(slug, _)| slug)
                }
                // giphy.com/gifs/<slug>-<id>
                "giphy.com" if segments.first() == Some(&"gifs") => {
                    segments.get(1)?.rsplit_once('-')?.0
                }
                _ => return None,
            };
            let description = slug.replace('-', " ");
            (!description.trim().is_empty()).then_some(description)
        })
        .collect()
}

/// Downloads an image and inlines it as a data URL
pub async fn fetch(http: &reqwest::Client, url: &str) -> anyhow::Result<ContentPart> {
    let response = http.get(url).send().await?.error_for_status()?;
    let mime = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("image/webp")
        .to_string();
    anyhow::ensure!(mime.starts_with("image/"), "Not an image: {mime}");
    let bytes = response.bytes().await?;
    anyhow::ensure!(bytes.len() <= MAX_DOWNLOAD_BYTES, "Image too large");

    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
    Ok(ContentPart::Image(ImageContentPart {
        image_url: ImageUrl {
            url: format!("data:{mime};base64,{data}"),
        },
    }))
}

/// Downloads the images that can be, logging the others
pub async fn fetch_all(http: &reqwest::Client, urls: &[String]) -> Vec<ContentPart> {
    let downloads = futures::future::join_all(urls.iter().map(|url| fetch(http, url))).await;
    downloads
        .into_iter()
        .filter_map(|download| {
            download
                .inspect_err(|err| tracing::warn!("Failed to download an image: {err:?}"))
                .ok()
        })
        .collect()
}

/// Describes the images of one message, in a few sentences for all of them
pub async fn caption(
    client: &openrouter::Client,
    model: &str,
    images: Vec<ContentPart>,
) -> anyhow::Result<String> {
    const SYSTEM_PROMPT: &str = "You describe images posted in a Discord chat for a chat bot \
        that can't see them. Describe what the images show in at most 80 words in total. If \
        there's text (memes, screenshots, signs), copy it out word for word first. Plain text, no \
        preamble.";

    anyhow::ensure!(!images.is_empty(), "No image to describe");
    let mut content = vec![ContentPart::Text(TextContentPart {
        text: "Describe these images.".to_string(),
    })];
    content.extend(images);

    let caption = client
        .complete(
            UsageKind::Caption,
            model,
            None,
            SYSTEM_PROMPT.to_string(),
            [Message {
                role: Role::User,
                content,
                tool_calls: Vec::new(),
                tool_call_id: None,
            }],
        )
        .await?;
    let caption = caption.split_whitespace().collect::<Vec<_>>().join(" ");
    anyhow::ensure!(!caption.is_empty(), "The model returned an empty caption");

    Ok(clamp_chars(&caption, MAX_CAPTION_CHARS))
}

fn clamp_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut clamped = text.chars().take(max_chars - 1).collect::<String>();
    clamped.push('…');
    clamped
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message_with(attachments: serde_json::Value) -> DiscordMessage {
        serde_json::from_value(serde_json::json!({
            "id": "1", "channel_id": "2", "type": 0, "content": "", "timestamp": "2026-10-31T00:00:00Z",
            "tts": false, "mention_everyone": false, "mentions": [], "mention_roles": [],
            "pinned": false, "embeds": [],
            "author": { "id": "3", "username": "globi", "discriminator": "0", "avatar": null },
            "attachments": attachments
        }))
        .unwrap()
    }

    fn attachment(
        id: u64,
        filename: &str,
        content_type: &str,
        size: Option<(u32, u32)>,
    ) -> serde_json::Value {
        let mut json = serde_json::json!({
            "id": id.to_string(), "filename": filename, "size": 1000, "content_type": content_type,
            "url": format!("https://cdn.discordapp.com/attachments/2/{id}/{filename}?ex=a&is=b&hm=c"),
            "proxy_url": format!("https://media.discordapp.net/attachments/2/{id}/{filename}?ex=a&is=b&hm=c"),
        });
        if let Some((width, height)) = size {
            json["width"] = width.into();
            json["height"] = height.into();
        }
        json
    }

    #[test]
    fn picks_images_and_resizes_them() {
        let message = message_with(serde_json::json!([
            attachment(10, "big.png", "image/png", Some((4000, 2000))),
            attachment(11, "small.gif", "image/gif", Some((220, 124))),
            attachment(12, "SPOILER_secret.png", "image/png", Some((100, 100))),
            attachment(13, "notes.txt", "text/plain", None),
        ]));
        let images = MessageImages::of(&message);

        assert_eq!(images.spoilers, 1);
        assert_eq!(
            images.urls,
            [
                "https://media.discordapp.net/attachments/2/10/big.png?ex=a&is=b&hm=c&width=1024&height=512&format=webp",
                "https://media.discordapp.net/attachments/2/11/small.gif?ex=a&is=b&hm=c&width=220&height=124&format=webp",
            ]
        );
        assert_eq!(images.placeholder(), "[Images]");
    }

    #[test]
    fn keeps_at_most_four_images() {
        let attachments = (1..=6)
            .map(|id| attachment(id, "a.png", "image/png", Some((10, 10))))
            .collect::<Vec<_>>();
        let images = MessageImages::of(&message_with(attachments.into()));
        assert_eq!(images.urls.len(), MAX_IMAGES_PER_MESSAGE);
    }

    #[test]
    fn fits_in_a_square() {
        assert_eq!(fit(4000, 2000, 1024), (1024, 512));
        assert_eq!(fit(500, 3000, 1024), (170, 1024));
        assert_eq!(fit(800, 600, 1024), (800, 600));
    }

    #[test]
    fn consent_comes_from_sharing_or_the_role() {
        let author = UserId::new(1);
        assert_eq!(consent(true, false, author, false), Some(Consent::Shared));
        assert_eq!(consent(false, true, author, true), Some(Consent::Shared));
        assert_eq!(
            consent(false, false, author, true),
            Some(Consent::Role(author))
        );
        assert_eq!(consent(false, false, author, false), None);
    }

    #[test]
    fn describes_gif_links() {
        let content = "lol https://tenor.com/view/cat-dancing-gif-12345 and \
            <https://tenor.com/fr/view/happy-dog-gif-999> \
            https://giphy.com/gifs/excited-yes-l0MYt5jPR6QX5pnqM \
            https://example.com/view/not-a-gif-1";
        assert_eq!(
            gif_descriptions(content),
            ["cat dancing", "happy dog", "excited yes"]
        );
    }
}
