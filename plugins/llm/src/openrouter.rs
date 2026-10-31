use itertools::Itertools;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone)]
pub struct Client {
    api_key: String,
    http_client: reqwest::Client,
}

impl Client {
    pub fn from_env() -> anyhow::Result<Self> {
        let api_key = std::env::var("LLM_OPENROUTER_API_KEY")?;

        Ok(Self {
            api_key,
            http_client: reqwest::Client::new(),
        })
    }

    pub fn complete<Parts: IntoIterator<Item = Message> + Send, SessionId: Into<Option<String>>>(
        &self,
        model: &str,
        session_id: SessionId,
        system_prompt: String,
        parts: Parts,
    ) -> impl Future<Output = anyhow::Result<String>> + Send + use<Parts, SessionId> {
        let mut messages = vec![Message::text(Role::System, system_prompt)];

        messages.extend(parts);

        let req = Request {
            model,
            messages,
            session_id: session_id.into(),
        };

        let request = self
            .http_client
            .post(COMPLETE_ENDPOINT)
            .bearer_auth(&self.api_key)
            .json(&req);

        async move {
            let response = request.send().await?.error_for_status()?;
            tracing::debug!("OpenRouter response: {response:?}");

            let chat_response: serde_json::Value = response.json().await?;
            tracing::debug!("OpenRouter chat response JSON: {chat_response:?}");

            let chat_response: Response = serde_json::from_value(chat_response)?;

            let answer = chat_response
                .choices
                .into_iter()
                .map(|c| c.message.content.unwrap_or_default())
                .join("");

            Ok(answer)
        }
    }
}

const COMPLETE_ENDPOINT: &str = "https://openrouter.ai/api/v1/chat/completions";

#[derive(Debug, Clone, Serialize)]
struct Request<'model> {
    model: &'model str,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    session_id: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct Response {
    choices: Vec<Choice>,
}

#[derive(Debug, Clone, Deserialize)]
struct Choice {
    message: ReponseMessage,
}

#[derive(Debug, Clone, Deserialize)]
struct ReponseMessage {
    // role: String,
    content: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
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
    }
}
