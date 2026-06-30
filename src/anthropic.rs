//! Minimal Anthropic Messages API client (blocking, via `ureq`/rustls) with
//! just the surface this agent needs: system prompt + tools + tool-use loop.

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// A content block, in either direction. `tool_result` is only ever sent by us;
/// `text` / `tool_use` come back from the model. Unknown future block types
/// deserialize to `Otro` so the loop never panics.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Block {
    Text {
        text: String,
    },
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    ToolResult {
        tool_use_id: String,
        content: String,
        #[serde(skip_serializing_if = "Option::is_none", default)]
        is_error: Option<bool>,
    },
    #[serde(other)]
    Otro,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Message {
    pub role: String,
    pub content: Vec<Block>,
}

impl Message {
    pub fn user_text(text: impl Into<String>) -> Self {
        Message {
            role: "user".into(),
            content: vec![Block::Text { text: text.into() }],
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MessageResponse {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub model: String,
    pub content: Vec<Block>,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub usage: Option<Usage>,
}

pub struct Client {
    api_key: String,
    base_url: String,
    version: String,
    agent: ureq::Agent,
}

impl Client {
    /// Build from `ANTHROPIC_API_KEY` (required). `ANTHROPIC_BASE_URL` optional.
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
            anyhow!("Falta ANTHROPIC_API_KEY. Defínela en el entorno o en un archivo .env.")
        })?;
        let base_url = std::env::var("ANTHROPIC_BASE_URL")
            .unwrap_or_else(|_| "https://api.anthropic.com".into());
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(10))
            .timeout_read(std::time::Duration::from_secs(120))
            .build();
        Ok(Client {
            api_key,
            base_url,
            version: "2023-06-01".into(),
            agent,
        })
    }

    pub fn create_message(
        &self,
        model: &str,
        max_tokens: u32,
        system: &str,
        tools: &[Value],
        messages: &[Message],
    ) -> Result<MessageResponse> {
        let body = json!({
            "model": model,
            "max_tokens": max_tokens,
            "system": system,
            "tools": tools,
            "messages": messages,
        });

        let resp = self
            .agent
            .post(&format!("{}/v1/messages", self.base_url))
            .set("x-api-key", &self.api_key)
            .set("anthropic-version", &self.version)
            .set("content-type", "application/json")
            .send_json(body);

        match resp {
            Ok(r) => r
                .into_json::<MessageResponse>()
                .context("decodificando la respuesta de Anthropic"),
            Err(ureq::Error::Status(code, r)) => {
                let detail = r.into_string().unwrap_or_default();
                Err(anyhow!("Anthropic API respondió {code}: {detail}"))
            }
            Err(e) => Err(anyhow!("error de transporte hacia Anthropic: {e}")),
        }
    }
}
