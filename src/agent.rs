//! The agent loop: send the conversation + enabled tools to the model, execute
//! any tool_use blocks against the local catalog, feed results back, repeat
//! until the model produces a plain-text reply.

use anyhow::Result;
use serde_json::{json, Value};

use crate::anthropic::{Block, Client, Message};
use crate::prompt;
use crate::tools::{self, AppState};

/// One executed tool call, surfaced for the `--debug` trace.
#[derive(Debug, Clone)]
pub struct TraceEntry {
    pub name: String,
    pub input: Value,
    pub output: Value,
}

/// Result of one user turn.
pub struct Turn {
    pub reply: String,
    pub trace: Vec<TraceEntry>,
}

pub struct Agent<'a> {
    state: &'a AppState,
    client: Client,
    model: String,
    max_tokens: u32,
    system: String,
    tool_defs: Vec<Value>,
    history: Vec<Message>,
    max_iters: usize,
}

impl<'a> Agent<'a> {
    pub fn new(state: &'a AppState, client: Client, model: String, max_tokens: u32) -> Self {
        let system = prompt::build(state);
        let tool_defs = tools::enabled_tools(state)
            .iter()
            .map(|t| {
                json!({
                    "name": t.name,
                    "description": t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect();
        Agent {
            state,
            client,
            model,
            max_tokens,
            system,
            tool_defs,
            history: Vec::new(),
            max_iters: 8,
        }
    }

    pub fn model(&self) -> &str {
        &self.model
    }

    pub fn tool_count(&self) -> usize {
        self.tool_defs.len()
    }

    /// Send a user message and drive the tool-use loop to a text reply.
    pub fn send(&mut self, user_msg: &str) -> Result<Turn> {
        self.history.push(Message::user_text(user_msg));
        let mut trace = Vec::new();

        for _ in 0..self.max_iters {
            let resp = self.client.create_message(
                &self.model,
                self.max_tokens,
                &self.system,
                &self.tool_defs,
                &self.history,
            )?;

            // Rebuild the assistant turn from text + tool_use blocks only
            // (drop any unknown block types before echoing back to the API).
            let mut assistant: Vec<Block> = Vec::new();
            let mut tool_uses: Vec<(String, String, Value)> = Vec::new();
            let mut texts: Vec<String> = Vec::new();

            for b in &resp.content {
                match b {
                    Block::Text { text } => {
                        texts.push(text.clone());
                        assistant.push(Block::Text { text: text.clone() });
                    }
                    Block::ToolUse { id, name, input } => {
                        tool_uses.push((id.clone(), name.clone(), input.clone()));
                        assistant.push(Block::ToolUse {
                            id: id.clone(),
                            name: name.clone(),
                            input: input.clone(),
                        });
                    }
                    _ => {}
                }
            }
            self.history.push(Message {
                role: "assistant".into(),
                content: assistant,
            });

            if tool_uses.is_empty() {
                return Ok(Turn {
                    reply: texts.join("\n").trim().to_string(),
                    trace,
                });
            }

            // Execute each tool call and return the results in one user turn.
            let mut results: Vec<Block> = Vec::new();
            for (id, name, input) in tool_uses {
                let output = tools::dispatch(self.state, &name, &input);
                trace.push(TraceEntry {
                    name: name.clone(),
                    input: input.clone(),
                    output: output.clone(),
                });
                results.push(Block::ToolResult {
                    tool_use_id: id,
                    content: output.to_string(),
                    is_error: None,
                });
            }
            self.history.push(Message {
                role: "user".into(),
                content: results,
            });
        }

        Ok(Turn {
            reply: "Disculpa, no pude completar la solicitud automáticamente. Te paso con una persona del equipo. 🙌".into(),
            trace,
        })
    }
}
