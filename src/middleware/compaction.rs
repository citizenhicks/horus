//! Context compaction policy and provider routing.

use std::sync::Arc;

use super::Middleware;
use super::ModelContext;
use super::approximate_item_tokens;
use serde_json::Value;
use uuid::Uuid;

use crate::BoxFuture;
use crate::Error;
use crate::Result;
use crate::backend::model::CompactOutput;
use crate::backend::model::CompactRequest;
use crate::backend::model::ModelRequest;
use crate::backend::model::internal_user_message;
use crate::backend::model::tool_complete_boundaries;
use crate::backend::model::user_message;
use crate::protocol::EventMsg;
use crate::protocol::FrontendBlock;
use crate::protocol::FrontendTone;
use crate::protocol::internal_message_kind;

const KEEP_RECENT_TOKENS: usize = 20_000;
const MAX_SUMMARY_TOOL_RESULT_CHARS: usize = 2_000;
const COMPACTION_RESERVE_TOKENS: i64 = 16_384;
/// Default compaction trigger for middleware instances without an override.
pub const DEFAULT_COMPACTION_TOKENS: i64 = 250_000;
const SUMMARY_SYSTEM_PROMPT: &str = "Summarize coding-agent history for continuation. Do not \
    continue the conversation. Output only the checkpoint.";
const SUMMARY_TASK: &str = "Create or update a concise checkpoint with: Goal; Constraints; \
    Progress (Done, In Progress, Blocked); Key Decisions; Next Steps; Critical Context. Preserve \
    exact paths, identifiers, commands, and errors.";

/// Compacts visible context after a configurable token threshold.
pub struct Compaction {
    at_tokens: i64,
}

impl Default for Compaction {
    fn default() -> Self {
        Self {
            at_tokens: DEFAULT_COMPACTION_TOKENS,
        }
    }
}

impl Compaction {
    /// Creates a threshold-based compaction policy.
    pub fn new(at_tokens: i64) -> Result<Self> {
        if at_tokens <= 0 {
            return Err(Error::Config(
                "compaction threshold must be positive".into(),
            ));
        }
        Ok(Self { at_tokens })
    }

    fn trigger_tokens(&self, context_window: i64) -> i64 {
        self.at_tokens
            .min(context_window.saturating_sub(COMPACTION_RESERVE_TOKENS))
            .max(1)
    }
}

impl Middleware for Compaction {
    fn name(&self) -> &'static str {
        "compaction"
    }

    fn render(&self, event: &EventMsg) -> Option<FrontendBlock> {
        matches!(event, EventMsg::ContextCompacted).then(|| FrontendBlock {
            id: None,
            group: None,
            append: false,
            pending: false,
            text: "context compacted".into(),
            format: crate::protocol::FrontendBlockFormat::PlainText,
            tone: FrontendTone::Neutral,
        })
    }

    fn before_model<'a>(&'a self, context: &'a mut ModelContext<'_>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let estimated = context.estimated_input_tokens();
            let observed = if starts_compacted(context.input()) {
                estimated
            } else {
                context
                    .last_usage
                    .map_or(0, |usage| usage.input_tokens)
                    .max(estimated)
            };
            if observed < self.trigger_tokens(context.context_window) || context.input().is_empty()
            {
                return Ok(());
            }
            let output = if context.model.compaction_endpoint(context.provider)? {
                let tools = context.tools.definitions();
                context
                    .model
                    .compact(
                        context.provider,
                        CompactRequest {
                            instructions: context.instructions,
                            input: context.input(),
                            tools: &tools,
                        },
                    )
                    .await?
            } else {
                summarize(context).await?
            };
            if output.output.is_empty() {
                return Err(Error::Provider(
                    "compaction returned an empty context".into(),
                ));
            }
            context.replace_input(output.output);
            context.usage.push(output.usage);
            context.events.push(EventMsg::ContextCompacted);
            Ok(())
        })
    }
}

async fn summarize(context: &ModelContext<'_>) -> Result<CompactOutput> {
    let (prompt, recent) = prepare_summary(context.input())
        .ok_or_else(|| Error::Provider("context has no safe history boundary to compact".into()))?;
    let session_id = Uuid::new_v4().to_string();
    let input = [user_message(&prompt)];
    let output = context
        .model
        .respond(
            context.provider,
            ModelRequest {
                session_id: &session_id,
                instructions: SUMMARY_SYSTEM_PROMPT,
                input: &input,
                tools: &[],
            },
            Arc::new(|_| Ok(())),
        )
        .await?;
    let summary = output.text().trim();
    if summary.is_empty() {
        return Err(Error::Provider(
            "model compaction returned no summary".into(),
        ));
    }
    let mut compacted = Vec::with_capacity(recent.len() + 1);
    compacted.push(internal_user_message(
        "compaction",
        &format!("<compacted_context>\n{summary}\n</compacted_context>"),
    ));
    compacted.extend(recent);
    CompactOutput::from_output(compacted, output.usage().clone())
}

fn prepare_summary(input: &[Value]) -> Option<(String, Vec<Value>)> {
    let cut = recent_cut(input, KEEP_RECENT_TOKENS)?;
    let prompt = summary_prompt(&input[..cut])?;
    Some((prompt, input[cut..].to_vec()))
}

fn recent_cut(input: &[Value], keep_tokens: usize) -> Option<usize> {
    let mut accumulated = 0;
    let mut desired = None;
    for index in (0..input.len()).rev() {
        accumulated += approximate_item_tokens(&input[index]);
        if accumulated >= keep_tokens {
            desired = Some(index);
            break;
        }
    }
    let desired = desired?;
    let safe = safe_boundaries(input);
    safe.iter()
        .rev()
        .copied()
        .find(|&index| index > 0 && index <= desired)
        .or_else(|| {
            safe.iter()
                .copied()
                .find(|&index| index > desired && index < input.len())
        })
}

fn safe_boundaries(input: &[Value]) -> Vec<usize> {
    tool_complete_boundaries(input)
        .into_iter()
        .filter(|&boundary| boundary == input.len() || safe_start(&input[boundary]))
        .collect()
}

fn safe_start(item: &Value) -> bool {
    match item.get("type").and_then(Value::as_str) {
        Some("function_call") => true,
        Some("message") | None => matches!(
            item.get("role").and_then(Value::as_str),
            Some("user" | "assistant")
        ),
        Some(_) => false,
    }
}

fn summary_prompt(history: &[Value]) -> Option<String> {
    let mut conversation = Vec::new();
    let mut previous_summary = None;
    for item in history {
        if let Some(summary) = compacted_summary(item) {
            previous_summary = Some(summary);
        } else if let Some(serialized) = serialize_item(item) {
            conversation.push(serialized);
        }
    }
    if conversation.is_empty() {
        return None;
    }
    let mut prompt = format!(
        "<conversation>\n{}\n</conversation>\n",
        conversation.join("\n\n")
    );
    if let Some(summary) = previous_summary {
        prompt.push_str(&format!(
            "\n<previous_summary>\n{summary}\n</previous_summary>\n"
        ));
    }
    prompt.push_str(&format!("\n{SUMMARY_TASK}"));
    Some(prompt)
}

fn serialize_item(item: &Value) -> Option<String> {
    match item.get("type").and_then(Value::as_str) {
        Some("function_call") => Some(format!(
            "[Assistant tool call]: {}({})",
            item.get("name").and_then(Value::as_str).unwrap_or("tool"),
            value_text(item.get("arguments"))
        )),
        Some("function_call_output") => Some(format!(
            "[Tool result]: {}",
            truncate_chars(
                &value_text(item.get("output")),
                MAX_SUMMARY_TOOL_RESULT_CHARS
            )
        )),
        Some("reasoning") => {
            let text = content_text(item.get("summary"));
            (!text.is_empty()).then(|| format!("[Assistant reasoning]: {text}"))
        }
        Some("message") | None => {
            let role = item.get("role").and_then(Value::as_str)?;
            let text = content_text(item.get("content"));
            (!text.is_empty()).then(|| {
                let label = if role == "assistant" {
                    "Assistant"
                } else {
                    "User"
                };
                format!("[{label}]: {text}")
            })
        }
        Some(_) => None,
    }
}

fn compacted_summary(item: &Value) -> Option<String> {
    if internal_message_kind(item) != Some("compaction") {
        return None;
    }
    let text = content_text(item.get("content"));
    text.strip_prefix("<compacted_context>")?
        .strip_suffix("</compacted_context>")
        .map(|summary| summary.trim().to_string())
}

fn starts_compacted(input: &[Value]) -> bool {
    input.first().is_some_and(|item| {
        item.get("type").and_then(Value::as_str) == Some("compaction")
            || compacted_summary(item).is_some()
    })
}

fn content_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|part| {
                part.get("text")
                    .or_else(|| part.get("content"))
                    .and_then(Value::as_str)
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Some(value) => value.to_string(),
        None => String::new(),
    }
}

fn value_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(value) => value.to_string(),
        None => String::new(),
    }
}

fn truncate_chars(text: &str, limit: usize) -> String {
    text.char_indices()
        .nth(limit)
        .map_or_else(|| text.to_string(), |(end, _)| format!("{}…", &text[..end]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::model::tool_output;

    #[test]
    fn recent_cut_keeps_parallel_calls_with_their_outputs() {
        let input = vec![
            user_message("old"),
            serde_json::json!({
                "type": "function_call",
                "call_id": "a",
                "name": "read",
                "arguments": "{}"
            }),
            serde_json::json!({
                "type": "function_call",
                "call_id": "b",
                "name": "read",
                "arguments": "{}"
            }),
            tool_output("a", &"x".repeat(200), false),
            tool_output("b", "done", false),
        ];

        assert_eq!(recent_cut(&input, 10), Some(1));
    }

    #[test]
    fn trigger_reserves_space_from_the_live_context_window() {
        let compaction = Compaction::default();

        assert_eq!(compaction.trigger_tokens(128_000), 111_616);
        assert_eq!(compaction.trigger_tokens(8_000), 1);
        assert_eq!(
            Compaction::new(4_000)
                .expect("custom threshold")
                .trigger_tokens(128_000),
            4_000
        );
    }
}
