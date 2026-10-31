use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use base64::Engine;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::usage::{Totals, UsageKind, UsageReport, UsageStats};

/// Records the usage of every completion it makes
#[derive(Debug, Clone)]
pub struct Client {
    api_key: String,
    http_client: reqwest::Client,
    usage: Arc<Mutex<UsageStats>>,
}

impl Client {
    pub fn from_env(usage: UsageStats) -> anyhow::Result<Self> {
        let api_key = std::env::var("LLM_OPENROUTER_API_KEY")?;

        Ok(Self {
            api_key,
            http_client: reqwest::Client::new(),
            usage: Arc::new(Mutex::new(usage)),
        })
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http_client
    }

    pub fn usage(&self) -> UsageStats {
        self.usage.lock().clone()
    }

    pub fn complete<Parts: IntoIterator<Item = Message> + Send, SessionId: Into<Option<String>>>(
        &self,
        kind: UsageKind,
        model: &str,
        session_id: SessionId,
        system_prompt: String,
        parts: Parts,
    ) -> impl Future<Output = anyhow::Result<String>> + Send + use<Parts, SessionId> {
        let mut messages = vec![Message::text(Role::System, system_prompt)];
        messages.extend(parts);

        let req = Request {
            model: model.to_string(),
            messages,
            session_id: session_id.into(),
            tools: None,
            tool_choice: None,
        };
        let client = self.clone();

        async move {
            let (message, _) = client.send(kind, &req).await?;
            Ok(message.content.unwrap_or_default())
        }
    }

    /// Like `complete`, but the model can call `tools`: each call is answered with
    /// `run_tool(name, json_arguments).await`, one at a time, until the model answers in text
    #[expect(
        clippy::too_many_arguments,
        reason = "same arguments as complete, plus the tools"
    )]
    pub async fn complete_with_tools<ToolRun: Future<Output = ToolOutput> + Send>(
        &self,
        kind: UsageKind,
        model: String,
        session_id: String,
        system_prompt: String,
        parts: Vec<Message>,
        tools: &[serde_json::Value],
        mut run_tool: impl FnMut(String, String) -> ToolRun + Send,
    ) -> anyhow::Result<ToolCompletion> {
        const MAX_TOOL_ROUNDS: usize = 5;

        let mut totals = Totals::default();

        let mut messages = vec![Message::text(Role::System, system_prompt)];
        messages.extend(parts);
        let mut req = Request {
            model,
            messages,
            session_id: Some(session_id),
            tools: Some(tools),
            tool_choice: None,
        };

        for round in 1..=MAX_TOOL_ROUNDS {
            // Out of rounds: the model must answer with what it has
            if round == MAX_TOOL_ROUNDS {
                req.tool_choice = Some("none");
            }

            let (message, round_totals) = self.send(kind, &req).await?;
            totals.add(&round_totals);
            if message.tool_calls.is_empty() {
                return Ok(ToolCompletion {
                    answer: message.content.unwrap_or_default(),
                    rounds: round,
                    totals,
                });
            }

            let mut results = Vec::new();
            let mut images = Vec::new();
            for call in &message.tool_calls {
                let output =
                    run_tool(call.function.name.clone(), call.function.arguments.clone()).await;
                images.extend(output.images);
                results.push(Message {
                    role: Role::Tool,
                    content: vec![ContentPart::Text(TextContentPart { text: output.text })],
                    tool_calls: Vec::new(),
                    tool_call_id: Some(call.id.clone()),
                });
            }
            // Most providers only take text in tool results: images follow as a user message
            if !images.is_empty() {
                let mut content = vec![ContentPart::Text(TextContentPart {
                    text: "[Images from the tool calls above, in order]".to_string(),
                })];
                content.extend(images);
                results.push(Message {
                    role: Role::User,
                    content,
                    tool_calls: Vec::new(),
                    tool_call_id: None,
                });
            }
            req.messages.push(Message {
                role: Role::Assistant,
                content: message
                    .content
                    .filter(|text| !text.is_empty())
                    .map(|text| ContentPart::Text(TextContentPart { text }))
                    .into_iter()
                    .collect(),
                tool_calls: message.tool_calls,
                tool_call_id: None,
            });
            req.messages.extend(results);
        }

        anyhow::bail!("The model kept calling tools after {MAX_TOOL_ROUNDS} rounds")
    }

    /// Sends one request and records its usage, which it also returns
    async fn send(
        &self,
        kind: UsageKind,
        req: &Request<'_>,
    ) -> anyhow::Result<(ReponseMessage, Totals)> {
        let images = req
            .messages
            .iter()
            .flat_map(|message| &message.content)
            .filter(|part| matches!(part, ContentPart::Image(_)))
            .count() as u64;

        let response = self
            .http_client
            .post(COMPLETE_ENDPOINT)
            .bearer_auth(&self.api_key)
            .json(req)
            .send()
            .await?;
        tracing::debug!("OpenRouter response: {response:?}");
        let status = response.status();
        if !status.is_success() {
            // OpenRouter says what's wrong in the body, e.g. a model without tool support
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("OpenRouter returned {status}: {body}");
        }

        let chat_response: serde_json::Value = response.json().await?;
        tracing::debug!("OpenRouter chat response JSON: {chat_response:?}");

        let chat_response: Response = serde_json::from_value(chat_response)?;

        let totals = match &chat_response.usage {
            Some(report) => {
                let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
                // The model that actually answered, in case OpenRouter picked a fallback
                let model = chat_response.model.as_deref().unwrap_or(&req.model);
                self.usage.lock().record(now, kind, model, images, report);
                Totals::from_report(report, images)
            }
            None => {
                tracing::warn!("OpenRouter response has no usage");
                Totals::default()
            }
        };

        let mut choices = chat_response.choices.into_iter();
        let mut message = choices
            .next()
            .context("OpenRouter response has no choices")?
            .message;
        for choice in choices {
            let content = message.content.get_or_insert_default();
            content.push_str(&choice.message.content.unwrap_or_default());
            message.tool_calls.extend(choice.message.tool_calls);
        }

        Ok((message, totals))
    }

    /// Makes one image with an image model, from a prompt and optional reference images (data
    /// URLs). Records its usage as `ImageGeneration`.
    pub async fn generate_image(
        &self,
        model: &str,
        prompt: &str,
        aspect_ratio: Option<&str>,
        references: Vec<String>,
    ) -> anyhow::Result<GeneratedImage> {
        #[derive(Deserialize)]
        struct ImagesResponse {
            data: Vec<ImageData>,
            usage: Option<UsageReport>,
        }
        #[derive(Deserialize)]
        struct ImageData {
            b64_json: String,
            media_type: Option<String>,
        }

        let reference_count = references.len() as u64;
        let mut req = serde_json::json!({
            "model": model,
            "prompt": prompt,
            "n": 1,
            // Discord shows them small anyway, and bigger ones cost more
            "resolution": "1K",
            "input_references": references
                .into_iter()
                .map(|url| serde_json::json!({ "type": "image_url", "image_url": { "url": url } }))
                .collect::<Vec<_>>(),
        });
        if let Some(aspect_ratio) = aspect_ratio {
            req["aspect_ratio"] = aspect_ratio.into();
        }

        let response = self
            .http_client
            .post(IMAGES_ENDPOINT)
            .bearer_auth(&self.api_key)
            .json(&req)
            .send()
            .await?;
        // Not the body: it's megabytes of base64
        tracing::debug!("OpenRouter image response status: {}", response.status());
        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            anyhow::bail!("OpenRouter returned {status}: {body}");
        }

        let found: ImagesResponse = response.json().await?;
        match &found.usage {
            Some(report) => {
                let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
                self.usage.lock().record(
                    now,
                    UsageKind::ImageGeneration,
                    model,
                    reference_count,
                    report,
                );
            }
            None => tracing::warn!("OpenRouter image response has no usage"),
        }

        let image = found
            .data
            .into_iter()
            .next()
            .context("OpenRouter returned no image")?;
        Ok(GeneratedImage {
            data: base64::engine::general_purpose::STANDARD.decode(image.b64_json)?,
            media_type: image.media_type.unwrap_or_else(|| "image/png".to_string()),
        })
    }

    /// The account's credits. The docs say this needs a management key, but a normal one works
    pub async fn credits(&self) -> anyhow::Result<Credits> {
        #[derive(Deserialize)]
        struct CreditsResponse {
            data: Credits,
        }

        let response = self
            .http_client
            .get(CREDITS_ENDPOINT)
            .bearer_auth(&self.api_key)
            .send()
            .await?
            .error_for_status()?;
        Ok(response.json::<CreditsResponse>().await?.data)
    }
}

/// In dollars, since the account was created
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Credits {
    pub total_credits: f64,
    pub total_usage: f64,
}

#[derive(Debug, Clone)]
pub struct GeneratedImage {
    pub data: Vec<u8>,
    /// `image/png`, `image/jpeg`, ...
    pub media_type: String,
}

impl GeneratedImage {
    /// A file name with the right extension, so Discord shows it inline
    pub fn file_name(&self) -> String {
        let extension = match self.media_type.as_str() {
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            _ => "png",
        };
        format!("image.{extension}")
    }
}

/// What a tool answers: text, and images to show the model
#[derive(Debug, Clone, Default)]
pub struct ToolOutput {
    pub text: String,
    /// Inline images: only with their owner's consent (`images::consent`)
    pub images: Vec<ContentPart>,
}

impl ToolOutput {
    pub fn text(text: String) -> Self {
        Self {
            text,
            images: Vec::new(),
        }
    }
}

/// The answer of `complete_with_tools`, with what it took to get it
#[derive(Debug, Clone)]
pub struct ToolCompletion {
    pub answer: String,
    /// Requests made: one per round of tool calls, plus the answer
    pub rounds: usize,
    /// Usage of every round added up
    pub totals: Totals,
}

const COMPLETE_ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";
const IMAGES_ENDPOINT: &str = "https://openrouter.ai/api/v1/images";
const CREDITS_ENDPOINT: &str = "https://openrouter.ai/api/v1/credits";

#[derive(Debug, Clone, Serialize)]
struct Request<'tools> {
    model: String,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<&'tools [serde_json::Value]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<&'static str>,
}

#[derive(Debug, Clone, Deserialize)]
struct Response {
    choices: Vec<Choice>,
    model: Option<String>,
    usage: Option<UsageReport>,
}

#[derive(Debug, Clone, Deserialize)]
struct Choice {
    message: ReponseMessage,
}

#[derive(Debug, Clone, Deserialize)]
struct ReponseMessage {
    // role: String,
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<ToolCall>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    // An assistant turn that only calls tools has no content
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub content: Vec<ContentPart>,
    // Defaulted so that contexts stored before tools existed still load
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub r#type: String,
    pub function: FunctionCall,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionCall {
    pub name: String,
    /// JSON encoded, as the model wrote it
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentPart {
    #[serde(rename = "text")]
    Text(TextContentPart),
    #[serde(rename = "image_url")]
    Image(ImageContentPart),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextContentPart {
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageContentPart {
    pub image_url: ImageUrl,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImageUrl {
    pub url: String,
}

impl Message {
    pub fn text(role: Role, text: String) -> Self {
        Self {
            role,
            content: vec![ContentPart::Text(TextContentPart { text })],
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_parts_use_openrouter_format() {
        let message = Message {
            role: Role::User,
            content: vec![
                ContentPart::Text(TextContentPart { text: "hi".into() }),
                ContentPart::Image(ImageContentPart {
                    image_url: ImageUrl {
                        url: "https://x".into(),
                    },
                }),
            ],
            tool_calls: Vec::new(),
            tool_call_id: None,
        };
        let json = serde_json::json!({
            "role": "user",
            "content": [
                { "type": "text", "text": "hi" },
                { "type": "image_url", "image_url": { "url": "https://x" } }
            ]
        });
        assert_eq!(serde_json::to_value(&message).unwrap(), json);
        let parsed = serde_json::from_value::<Message>(json).unwrap();
        assert!(matches!(&parsed.content[0], ContentPart::Text(part) if part.text == "hi"));
        assert!(parsed.tool_calls.is_empty() && parsed.tool_call_id.is_none());
    }

    #[test]
    fn tool_turns_use_openrouter_format() {
        let call = Message {
            role: Role::Assistant,
            content: Vec::new(),
            tool_calls: vec![ToolCall {
                id: "call_1".into(),
                r#type: "function".into(),
                function: FunctionCall {
                    name: "get_lore".into(),
                    arguments: r#"{"user_ids":["1"]}"#.into(),
                },
            }],
            tool_call_id: None,
        };
        assert_eq!(
            serde_json::to_value(&call).unwrap(),
            serde_json::json!({
                "role": "assistant",
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "get_lore", "arguments": "{\"user_ids\":[\"1\"]}" }
                }]
            })
        );

        let result = Message {
            tool_call_id: Some("call_1".into()),
            ..Message::text(Role::Tool, "Likes cats".into())
        };
        assert_eq!(
            serde_json::to_value(&result).unwrap(),
            serde_json::json!({
                "role": "tool",
                "content": [{ "type": "text", "text": "Likes cats" }],
                "tool_call_id": "call_1"
            })
        );
    }
}
