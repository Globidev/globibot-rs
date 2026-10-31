use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::Context;
use parking_lot::Mutex;
use serde::{Deserialize, Serialize};

use crate::usage::{UsageKind, UsageReport, UsageStats};

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
            let message = client.send(kind, &req).await?;
            Ok(message.content.unwrap_or_default())
        }
    }

    /// Like `complete`, but the model can call `tools`: each call is answered with
    /// `run_tool(name, json_arguments)` until the model answers in text
    #[expect(
        clippy::too_many_arguments,
        reason = "same arguments as complete, plus the tools"
    )]
    pub async fn complete_with_tools(
        &self,
        kind: UsageKind,
        model: String,
        session_id: String,
        system_prompt: String,
        parts: Vec<Message>,
        tools: &[serde_json::Value],
        mut run_tool: impl FnMut(&str, &str) -> String + Send,
    ) -> anyhow::Result<String> {
        const MAX_TOOL_ROUNDS: usize = 5;

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

            let message = self.send(kind, &req).await?;
            if message.tool_calls.is_empty() {
                return Ok(message.content.unwrap_or_default());
            }

            let results = message
                .tool_calls
                .iter()
                .map(|call| Message {
                    role: Role::Tool,
                    content: vec![ContentPart::Text(TextContentPart {
                        text: run_tool(&call.function.name, &call.function.arguments),
                    })],
                    tool_calls: Vec::new(),
                    tool_call_id: Some(call.id.clone()),
                })
                .collect::<Vec<_>>();
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

    /// Sends one request and records its usage
    async fn send(&self, kind: UsageKind, req: &Request<'_>) -> anyhow::Result<ReponseMessage> {
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

        match &chat_response.usage {
            Some(report) => {
                let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
                // The model that actually answered, in case OpenRouter picked a fallback
                let model = chat_response.model.as_deref().unwrap_or(&req.model);
                self.usage.lock().record(now, kind, model, images, report);
            }
            None => tracing::warn!("OpenRouter response has no usage"),
        }

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

        Ok(message)
    }
}

const COMPLETE_ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";

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
