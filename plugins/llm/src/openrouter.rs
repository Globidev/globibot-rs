use itertools::Itertools;
use serde::{Deserialize, Serialize};

use crate::personality::Personality;

pub struct Client {
    api_key: String,
    pub model: String,
    pub personality: Personality,
    http_client: reqwest::Client,
}

impl Client {
    pub fn from_env() -> anyhow::Result<Self> {
        let api_key = std::env::var("LLM_OPENROUTER_API_KEY")?;
        let model = std::env::var("LLM_DEFAULT_MODEL_ID")?;

        Ok(Self {
            api_key,
            model,
            personality: <_>::default(),
            http_client: reqwest::Client::new(),
        })
    }

    pub fn complete<Parts: IntoIterator<Item = Message> + Send>(
        &self,
        parts: Parts,
    ) -> impl Future<Output = anyhow::Result<String>> + Send + use<Parts> {
        let mut messages = vec![Message {
            role: Role::System,
            content: vec![ContentPart::Text(TextContentPart {
                kind: "text",
                text: self.personality.system_prompt(),
            })],
        }];

        messages.extend(parts);

        let req = Request {
            model: &self.model,
            messages,
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

#[derive(Debug, Clone, Serialize)]
pub struct Message {
    pub role: Role,
    pub content: Vec<ContentPart>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
}

#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum ContentPart {
    Text(TextContentPart),
    Image(ImageContentPart),
}

#[derive(Debug, Clone, Serialize)]
pub struct TextContentPart {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub text: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImageContentPart {
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub image_url: ImageUrl,
}

#[derive(Debug, Clone, Serialize)]
pub struct ImageUrl {
    pub url: String,
}
