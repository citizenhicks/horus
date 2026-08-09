use super::*;
use crate::backend::model::REPLAY_REASONING_FIELD;

#[test]
fn base_url_rejects_serializable_secret_locations() {
    for url in [
        "https://secret@example.com/v1",
        "https://example.com/v1?key=secret",
        "https://example.com/v1#secret",
    ] {
        assert!(OpenAi::new("test-key", url, "test-model").is_err());
    }
}

#[test]
fn responses_input_strips_only_top_level_provider_metadata() {
    let input = vec![
        serde_json::json!({
            "type": "function_call",
            "arguments": {"_keep": true},
            "_horus_reasoning": "Plan.",
            "_provider_internal": [{"type": "thinking"}]
        }),
        serde_json::json!({
            "type": "reasoning",
            "encrypted_content": "opaque",
            "format": "openai-responses-v1",
            "summary": []
        }),
    ];

    assert_eq!(
        wire_input(&input, true).expect("wire input"),
        vec![
            serde_json::json!({
                "type": "function_call",
                "arguments": {"_keep": true}
            }),
            serde_json::json!({
                "type": "reasoning",
                "encrypted_content": "opaque",
                "summary": []
            }),
        ]
    );
}

#[test]
fn responses_decode_strips_reasoning_wire_metadata() {
    let decoded = decode_response(serde_json::json!({
        "output": [{
            "type": "reasoning",
            "format": "openai-responses-v1",
            "summary": [{"type": "summary_text", "text": "Plan."}]
        }]
    }))
    .expect("decode response");

    assert_eq!(decoded.output()[0].get("format"), None);
    assert_eq!(decoded.output()[0][REPLAY_REASONING_FIELD], "Plan.");
}

#[test]
fn responses_converts_neutral_images_and_rejects_them_when_disabled() {
    let input = [serde_json::json!({
        "role": "user",
        "content": [
            {"type": "input_text", "text": "What is this?"},
            {"type": "input_image", "media_type": "image/png", "data": "aGVsbG8="}
        ]
    })];

    let wired = wire_input(&input, true).expect("wire image");
    assert_eq!(
        wired[0]["content"][1],
        serde_json::json!({
            "type": "input_image",
            "image_url": "data:image/png;base64,aGVsbG8="
        })
    );
    assert!(
        wire_input(&input, false)
            .expect_err("disabled image input")
            .to_string()
            .contains("does not support image attachments")
    );
}

#[test]
fn hosted_tools_can_be_disabled_per_request() {
    let hosted = [serde_json::json!({"type": "web_search"})];

    assert!(wire_tools(&[], &hosted, false).is_empty());
    assert_eq!(wire_tools(&[], &hosted, true), hosted);
}

#[test]
fn responses_decode_preserves_reasoning_content_for_replay() {
    let decoded = decode_response(serde_json::json!({
        "output": [{
            "type": "reasoning",
            "content": [{"type": "reasoning_text", "text": "Plan."}]
        }]
    }))
    .expect("decode response");

    assert_eq!(decoded.output()[0][REPLAY_REASONING_FIELD], "Plan.");
}

#[test]
fn responses_decode_normalizes_tool_calls_usage_and_errors() {
    let decoded = decode_response(serde_json::json!({
        "output": [
            {
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": "Checking."}]
            },
            {
                "type": "function_call",
                "call_id": "call-1",
                "name": "read",
                "arguments": "{\"path\":\"README.md\"}"
            }
        ],
        "usage": {
            "input_tokens": 10,
            "input_tokens_details": {"cached_tokens": 4},
            "output_tokens": 3,
            "output_tokens_details": {"reasoning_tokens": 1},
            "total_tokens": 13
        }
    }))
    .expect("decode response");

    assert_eq!(decoded.text(), "Checking.");
    assert_eq!(decoded.tool_calls()[0].arguments["path"], "README.md");
    assert_eq!(decoded.usage().cached_input_tokens, 4);
    assert_eq!(
        response_error(&serde_json::json!({"error": {"message": "bad request"}})),
        "bad request"
    );
}

#[test]
fn responses_emits_reasoning_text_deltas() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink_seen = Arc::clone(&seen);
    let events: ModelEventSink = Arc::new(move |event| {
        sink_seen.lock().expect("events lock").push(event);
        Ok(())
    });

    assert!(
        emit_reasoning_event(
            &serde_json::json!({
                "type": "response.reasoning_text.delta",
                "delta": "Plan."
            }),
            &events,
        )
        .expect("reasoning event")
    );
    assert_eq!(
        *seen.lock().expect("events lock"),
        vec![ModelEvent::ReasoningDelta("Plan.".into())]
    );
}

#[test]
fn compaction_shape_is_provider_neutral_and_opt_in() {
    let provider =
        OpenAi::new("test-key", "https://api.openai.com/v1", "test-model").expect("provider");
    assert!(!provider.compaction_endpoint());
    let input = [serde_json::json!({
        "role": "user",
        "content": "hello",
        "_private": true
    })];
    let tools = [ToolDefinition {
        name: "read".into(),
        description: "Read".into(),
        parameters: serde_json::json!({"type": "object"}),
    }];
    let body = provider
        .with_compaction_endpoint()
        .compact_body(CompactRequest {
            instructions: "compact",
            input: &input,
            tools: &tools,
        })
        .expect("compact body");

    assert_eq!(body["instructions"], "compact");
    assert_eq!(body["input"][0].get("_private"), None);
    assert_eq!(body["tools"][0]["name"], "read");
    assert_eq!(body["parallel_tool_calls"], true);
}
