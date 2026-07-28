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
    let input = vec![serde_json::json!({
        "type": "function_call",
        "arguments": {"_keep": true},
        "_horus_reasoning": "Plan.",
        "_provider_internal": [{"type": "thinking"}]
    })];

    assert_eq!(
        wire_input(&input),
        vec![serde_json::json!({
            "type": "function_call",
            "arguments": {"_keep": true}
        })]
    );

    let decoded = decode_response(serde_json::json!({
        "output": [{
            "type": "reasoning",
            "summary": [{"type": "summary_text", "text": "Plan."}]
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
        });

    assert_eq!(body["instructions"], "compact");
    assert_eq!(body["input"][0].get("_private"), None);
    assert_eq!(body["tools"][0]["name"], "read");
    assert_eq!(body["parallel_tool_calls"], true);
}
