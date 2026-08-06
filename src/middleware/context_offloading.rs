//! Durable masking of stale tool output in active model context.

use serde_json::Value;

use super::manifest::{MiddlewareManifest, MiddlewareSettingManifest};
use super::{Middleware, ModelContext, approximate_item_tokens};
use crate::protocol::{TOOL_ERROR_FIELD, is_internal_message};
use crate::{BoxFuture, Error, Result};

const MASKED_TOOL_OUTPUT: &str = "[offloaded]";

/// Default trailing token window retained by context offloading.
pub const DEFAULT_STALE_AFTER_TOKENS: i64 = 50_000;
const SETTINGS: &[MiddlewareSettingManifest] = &[MiddlewareSettingManifest::Integer {
    id: "stale_after_tokens",
    label: "Stale after tokens",
    description: "Successful tool results older than this trailing window are masked",
    min: 1,
    max: None,
    step: 10_000,
    default: DEFAULT_STALE_AFTER_TOKENS,
}];

/// Configuration and presentation metadata for context offloading.
pub const MANIFEST: MiddlewareManifest = MiddlewareManifest {
    id: "context_offloading",
    label: "Context offloading",
    description: "Mask stale successful tool output from active model context",
    required: false,
    default_enabled: true,
    settings: SETTINGS,
};

/// Masks successful tool output older than a trailing token window.
pub struct ContextOffloading {
    stale_after_tokens: usize,
}

impl ContextOffloading {
    /// Creates a tool-output retention policy.
    pub fn new(stale_after_tokens: i64) -> Result<Self> {
        let stale_after_tokens = usize::try_from(stale_after_tokens)
            .map_err(|_| Error::Config("context offloading threshold must be positive".into()))?;
        if stale_after_tokens == 0 {
            return Err(Error::Config(
                "context offloading threshold must be positive".into(),
            ));
        }
        Ok(Self { stale_after_tokens })
    }
}

impl Middleware for ContextOffloading {
    fn name(&self) -> &'static str {
        MANIFEST.id
    }

    fn before_model<'a>(&'a self, context: &'a mut ModelContext<'_>) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if let Some(input) = mask_stale_outputs(context.input(), self.stale_after_tokens) {
                context.replace_input(input);
            }
            Ok(())
        })
    }
}

fn mask_stale_outputs(input: &[Value], stale_after_tokens: usize) -> Option<Vec<Value>> {
    let latest_user = input.iter().rposition(|item| {
        item.get("role").and_then(Value::as_str) == Some("user") && !is_internal_message(item)
    })?;
    let mut newer_tokens = 0;
    let mut stale = Vec::new();

    for (index, item) in input.iter().enumerate().rev() {
        if index < latest_user
            && newer_tokens >= stale_after_tokens
            && successful_tool_output(item).is_some_and(|output| output != MASKED_TOOL_OUTPUT)
        {
            stale.push(index);
        }
        newer_tokens = newer_tokens.saturating_add(approximate_item_tokens(item));
    }

    if stale.is_empty() {
        return None;
    }
    let mut masked = input.to_vec();
    for index in stale {
        masked[index]["output"] = Value::String(MASKED_TOOL_OUTPUT.into());
    }
    Some(masked)
}

fn successful_tool_output(item: &Value) -> Option<&str> {
    if item.get("type").and_then(Value::as_str) != Some("function_call_output")
        || item.get(TOOL_ERROR_FIELD).and_then(Value::as_bool) == Some(true)
    {
        return None;
    }
    item.get("output").and_then(Value::as_str)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::model::{tool_output, user_message};

    #[test]
    fn masks_only_stale_successful_outputs_before_the_latest_user() {
        let stale_output = "stale".repeat(100);
        let failed_output = "failed".repeat(100);
        let recent_output = "recent".repeat(100);
        let active_output = "active".repeat(100);
        let input = vec![
            user_message("old turn"),
            serde_json::json!({
                "type": "function_call",
                "call_id": "stale",
                "name": "read",
                "arguments": "{}"
            }),
            tool_output("stale", &stale_output, false),
            serde_json::json!({
                "type": "function_call",
                "call_id": "failed",
                "name": "read",
                "arguments": "{}"
            }),
            tool_output("failed", &failed_output, true),
            serde_json::json!({"role": "assistant", "content": "padding".repeat(200)}),
            serde_json::json!({
                "type": "function_call",
                "call_id": "recent",
                "name": "read",
                "arguments": "{}"
            }),
            tool_output("recent", &recent_output, false),
            user_message("latest turn"),
            serde_json::json!({
                "type": "function_call",
                "call_id": "active",
                "name": "read",
                "arguments": "{}"
            }),
            tool_output("active", &active_output, false),
        ];
        let latest_user = input
            .iter()
            .rposition(|item| item.get("role").and_then(Value::as_str) == Some("user"))
            .expect("latest user");
        let recent_age = input[latest_user..]
            .iter()
            .map(approximate_item_tokens)
            .sum::<usize>();

        let masked = mask_stale_outputs(&input, recent_age + 10).expect("stale output");

        assert_eq!(masked[2]["output"], MASKED_TOOL_OUTPUT);
        assert_eq!(masked[4]["output"], failed_output);
        assert_eq!(masked[7]["output"], recent_output);
        assert_eq!(masked[10]["output"], active_output);
        for (index, (before, after)) in input.iter().zip(&masked).enumerate() {
            if index != 2 {
                assert_eq!(before, after);
            }
        }
        assert!(mask_stale_outputs(&masked, recent_age + 10).is_none());
        assert!(mask_stale_outputs(&input[1..latest_user], 1).is_none());
    }
}
