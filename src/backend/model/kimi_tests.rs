use serde_json::json;

use super::*;
use crate::backend::model::user_message;

#[test]
fn responses_history_becomes_kimi_messages_and_tools() {
    let provider = Kimi::new("test-key", "kimi-k3")
        .expect("provider")
        .with_reasoning_effort("high")
        .expect("reasoning");
    let input = vec![
        user_message("Inspect both files."),
        json!({
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "I will inspect them."}],
            (REPLAY_REASONING_FIELD): "Use parallel reads."
        }),
        json!({
            "type": "function_call",
            "call_id": "call-a",
            "name": "read",
            "arguments": "{\"path\":\"a.rs\"}"
        }),
        json!({
            "type": "function_call",
            "call_id": "call-b",
            "name": "read",
            "arguments": "{\"path\":\"b.rs\"}"
        }),
        json!({"type": "function_call_output", "call_id": "call-a", "output": "A"}),
        json!({"type": "function_call_output", "call_id": "call-b", "output": "B"}),
    ];
    let tools = [ToolDefinition {
        name: "read".into(),
        description: "Read a file".into(),
        parameters: json!({
            "type": "object",
            "properties": {"path": {"type": "string"}},
            "required": ["path"]
        }),
    }];
    let request = ModelRequest {
        session_id: "session-7",
        instructions: "Be precise.",
        input: &input,
        tools: &tools,
    };

    let body = provider.request_body(&request).expect("request body");

    assert_eq!(body["model"], "kimi-k3");
    assert_eq!(body["reasoning_effort"], "high");
    assert_eq!(body["prompt_cache_key"], "session-7");
    assert_eq!(body["stream_options"], json!({"include_usage": true}));
    assert_eq!(body["parallel_tool_calls"], true);
    assert_eq!(
        body["messages"],
        json!([
            {"role": "system", "content": "Be precise."},
            {"role": "user", "content": "Inspect both files."},
            {
                "role": "assistant",
                "content": "I will inspect them.",
                "reasoning_content": "Use parallel reads.",
                "tool_calls": [
                    {
                        "id": "call-a",
                        "type": "function",
                        "function": {"name": "read", "arguments": "{\"path\":\"a.rs\"}"}
                    },
                    {
                        "id": "call-b",
                        "type": "function",
                        "function": {"name": "read", "arguments": "{\"path\":\"b.rs\"}"}
                    }
                ]
            },
            {"role": "tool", "tool_call_id": "call-a", "content": "A"},
            {"role": "tool", "tool_call_id": "call-b", "content": "B"}
        ])
    );
    assert_eq!(
        body["tools"],
        json!([{
            "type": "function",
            "function": {
                "name": "read",
                "description": "Read a file",
                "parameters": {
                    "type": "object",
                    "properties": {"path": {"type": "string"}},
                    "required": ["path"]
                }
            }
        }])
    );
}

#[test]
fn stream_normalizes_deltas_tools_usage_and_errors() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink_seen = Arc::clone(&seen);
    let events: ModelEventSink = Arc::new(move |event| {
        sink_seen.lock().expect("events lock").push(event);
        Ok(())
    });
    let mut stream = StreamState::default();
    stream
        .apply_data(
            &json!({
                "choices": [{
                    "delta": {
                        "reasoning_content": "Plan.",
                        "content": "Reading.",
                        "tool_calls": [{
                            "index": 0,
                            "id": "call-1",
                            "function": {"name": "read", "arguments": "{\"path\":"}
                        }]
                    }
                }]
            })
            .to_string(),
            &events,
        )
        .expect("first delta");
    stream
        .apply_data(
            &json!({
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "function": {"arguments": "\"README.md\"}"}
                        }]
                    }
                }],
                "usage": {
                    "prompt_tokens": 10,
                    "prompt_tokens_details": {"cached_tokens": 4},
                    "completion_tokens": 3,
                    "total_tokens": 13
                }
            })
            .to_string(),
            &events,
        )
        .expect("second delta");
    stream.apply_data("[DONE]", &events).expect("done");

    let output = stream.finish().expect("normalized output");
    assert_eq!(output.text(), "Reading.");
    assert_eq!(output.tool_calls()[0].arguments["path"], "README.md");
    assert_eq!(output.usage().cached_input_tokens, 4);
    assert!(matches!(
        seen.lock().expect("events lock").as_slice(),
        [ModelEvent::ReasoningDelta(_), ModelEvent::TextDelta(_)]
    ));

    let mut failed = StreamState::default();
    let error = failed
        .apply_data(r#"{"error":{"message":"quota"}} "#, &events)
        .expect_err("stream error");
    assert!(error.to_string().contains("quota"));
}
