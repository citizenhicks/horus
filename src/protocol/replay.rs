//! Converts neutral model history into frontend presentation events.

use std::collections::BTreeMap;

use serde_json::Value;

use crate::protocol::AgentMessageEvent;
use crate::protocol::AgentMessagePhase;
use crate::protocol::AgentReasoningContentDeltaEvent;
use crate::protocol::EventMsg;
use crate::protocol::ToolCallBeginEvent;
use crate::protocol::ToolCallEndEvent;
use crate::protocol::UserMessageEvent;

pub(crate) const INTERNAL_MESSAGE_FIELD: &str = "_horus_internal";
pub(crate) const REPLAY_REASONING_FIELD: &str = "_horus_reasoning";
pub(crate) const TOOL_ERROR_FIELD: &str = "_horus_is_error";

pub(crate) fn internal_message_kind(message: &Value) -> Option<&str> {
    message.get(INTERNAL_MESSAGE_FIELD)?.as_str()
}

pub(crate) fn is_internal_message(message: &Value) -> bool {
    internal_message_kind(message).is_some()
}

pub(crate) fn events(context: &[Value], session_id: &str) -> Vec<EventMsg> {
    let mut events = Vec::new();
    let mut tools = BTreeMap::new();
    let mut turn = 0;
    let mut item = 0;
    for value in context {
        let turn_id = format!("history-{turn}");
        if let Some(text) = message_text(value, "user") {
            if !is_internal_message(value) {
                turn += 1;
                events.push(EventMsg::UserMessage(UserMessageEvent { message: text }));
            }
            continue;
        }
        if value.get("role").and_then(Value::as_str) == Some("assistant") {
            push_reasoning(
                &mut events,
                reasoning_text(value),
                session_id,
                &turn_id,
                item,
            );
            if let Some(message) = message_text(value, "assistant") {
                events.push(EventMsg::AgentMessage(AgentMessageEvent {
                    message,
                    phase: Some(AgentMessagePhase::FinalAnswer),
                }));
            }
            item += 1;
            continue;
        }
        match value.get("type").and_then(Value::as_str) {
            Some("reasoning") => {
                push_reasoning(
                    &mut events,
                    reasoning_text(value),
                    session_id,
                    &turn_id,
                    item,
                );
                item += 1;
            }
            Some("function_call") => {
                let call_id = string(value, "call_id");
                let name = string(value, "name");
                if call_id.is_empty() || name.is_empty() {
                    continue;
                }
                tools.insert(call_id.clone(), name.clone());
                events.push(EventMsg::ToolCallBegin(ToolCallBeginEvent {
                    turn_id,
                    call_id,
                    name,
                    arguments: arguments(value.get("arguments")),
                }));
            }
            Some("function_call_output") => {
                let call_id = string(value, "call_id");
                let output = value_text(value.get("output"));
                events.push(EventMsg::ToolCallEnd(ToolCallEndEvent {
                    turn_id,
                    name: tools
                        .get(&call_id)
                        .cloned()
                        .unwrap_or_else(|| "tool".into()),
                    call_id,
                    is_error: value
                        .get(TOOL_ERROR_FIELD)
                        .and_then(Value::as_bool)
                        .unwrap_or(false),
                    output,
                }));
            }
            Some(_) | None => {}
        }
    }
    events
}

fn push_reasoning(
    events: &mut Vec<EventMsg>,
    reasoning: Option<String>,
    session_id: &str,
    turn_id: &str,
    item: usize,
) {
    let Some(delta) = reasoning.filter(|reasoning| !reasoning.trim().is_empty()) else {
        return;
    };
    events.push(EventMsg::AgentReasoningContentDelta(
        AgentReasoningContentDeltaEvent {
            thread_id: session_id.into(),
            turn_id: turn_id.into(),
            item_id: format!("history-{item}"),
            delta,
        },
    ));
}

fn message_text(value: &Value, role: &str) -> Option<String> {
    if value.get("role").and_then(Value::as_str) != Some(role) {
        return None;
    }
    let content = value.get("content")?;
    match content {
        Value::String(text) => Some(text.clone()),
        Value::Array(parts) => {
            let text: String = parts
                .iter()
                .filter_map(|part| part.get("text").and_then(Value::as_str))
                .collect();
            (!text.is_empty()).then_some(text)
        }
        _ => None,
    }
}

fn reasoning_text(value: &Value) -> Option<String> {
    value
        .get(REPLAY_REASONING_FIELD)
        .and_then(Value::as_str)
        .map(str::to_string)
}

fn string(value: &Value, field: &str) -> String {
    value
        .get(field)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

fn arguments(value: Option<&Value>) -> Value {
    match value {
        Some(Value::String(value)) => {
            serde_json::from_str(value).unwrap_or_else(|_| Value::String(value.clone()))
        }
        Some(value) => value.clone(),
        None => serde_json::json!({}),
    }
}

fn value_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(value)) => value.clone(),
        Some(value) => value.to_string(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::model::internal_user_message;

    #[test]
    fn replay_uses_only_neutral_reasoning_and_hides_internal_messages() {
        let history = [
            serde_json::json!({"role": "user", "content": "hello"}),
            serde_json::json!({
                "role": "assistant",
                "content": "done",
                "_horus_reasoning": "neutral",
                "_anthropic_content": "provider-private"
            }),
            internal_user_message("compaction", "hidden"),
        ];

        let replayed = events(&history, "session");

        assert_eq!(replayed.len(), 3);
        assert!(matches!(&replayed[0], EventMsg::UserMessage(event) if event.message == "hello"));
        assert!(matches!(
            &replayed[1],
            EventMsg::AgentReasoningContentDelta(event) if event.delta == "neutral"
        ));
        assert!(matches!(
            &replayed[2],
            EventMsg::AgentMessage(event) if event.message == "done"
        ));
    }
}
