use super::*;
use crate::backend::model::user_message;

#[test]
fn responses_history_translates_to_anthropic_tool_messages() {
    let messages = translate_messages(&[
        user_message("inspect it"),
        serde_json::json!({
            "type": "message",
            "role": "assistant",
            "content": [{"type": "output_text", "text": "Checking."}]
        }),
        serde_json::json!({
            "type": "function_call",
            "call_id": "call_1",
            "name": "read_file",
            "arguments": "{\"path\":\"README.md\"}"
        }),
        serde_json::json!({
            "type": "function_call_output",
            "call_id": "call_1",
            "output": "contents"
        }),
    ])
    .expect("translate history");

    assert_eq!(
        messages,
        vec![
            serde_json::json!({
                "role": "user",
                "content": [{"type": "text", "text": "inspect it"}]
            }),
            serde_json::json!({
                "role": "assistant",
                "content": [
                    {"type": "text", "text": "Checking."},
                    {
                        "type": "tool_use",
                        "id": "call_1",
                        "name": "read_file",
                        "input": {"path": "README.md"}
                    }
                ]
            }),
            serde_json::json!({
                "role": "user",
                "content": [{
                    "type": "tool_result",
                    "tool_use_id": "call_1",
                    "content": "contents",
                    "is_error": false
                }]
            })
        ]
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
    for event in [
        serde_json::json!({
            "type": "message_start",
            "message": {"usage": {
                "input_tokens": 6,
                "cache_read_input_tokens": 4,
                "cache_creation_input_tokens": 2
            }}
        }),
        serde_json::json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "text", "text": ""}
        }),
        serde_json::json!({
            "type": "content_block_delta",
            "index": 0,
            "delta": {"type": "text_delta", "text": "Reading."}
        }),
        serde_json::json!({
            "type": "content_block_start",
            "index": 1,
            "content_block": {"type": "thinking", "thinking": ""}
        }),
        serde_json::json!({
            "type": "content_block_delta",
            "index": 1,
            "delta": {"type": "thinking_delta", "thinking": "Plan."}
        }),
        serde_json::json!({
            "type": "content_block_start",
            "index": 2,
            "content_block": {
                "type": "tool_use",
                "id": "call-1",
                "name": "read",
                "input": {}
            }
        }),
        serde_json::json!({
            "type": "content_block_delta",
            "index": 2,
            "delta": {"type": "input_json_delta", "partial_json": "{\"path\":\"README.md\"}"}
        }),
        serde_json::json!({"type": "content_block_stop", "index": 2}),
        serde_json::json!({
            "type": "message_delta",
            "delta": {"stop_reason": "tool_use"},
            "usage": {"output_tokens": 3}
        }),
        serde_json::json!({"type": "message_stop"}),
    ] {
        stream.apply(event, &events).expect("stream event");
    }

    let output = stream.finish().expect("normalized output");
    assert_eq!(output.text(), "Reading.");
    assert_eq!(output.tool_calls()[0].arguments["path"], "README.md");
    assert_eq!(output.usage().input_tokens, 12);
    assert_eq!(output.usage().cached_input_tokens, 4);
    assert!(matches!(
        seen.lock().expect("events lock").as_slice(),
        [ModelEvent::TextDelta(_), ModelEvent::ReasoningDelta(_)]
    ));

    let error = StreamState::default()
        .apply(
            serde_json::json!({"type": "error", "error": {"message": "quota"}}),
            &events,
        )
        .expect_err("stream error");
    assert!(error.to_string().contains("quota"));
}

#[test]
fn usage_rejects_provider_integer_overflow() {
    let usage = Usage {
        input: i64::MAX,
        cache_read: 1,
        ..Usage::default()
    };

    assert!(usage.finish().is_err());
}
