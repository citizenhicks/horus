use super::*;
use crate::backend::model::REPLAY_REASONING_FIELD;

fn model_request() -> ModelRequest<'static> {
    ModelRequest {
        session_id: "test-session",
        instructions: "Test instructions",
        input: &[],
        tools: &[],
        allow_hosted_tools: false,
        allow_continuation: false,
    }
}

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
fn endpoint_url_does_not_infer_reasoning_summary_support() {
    let provider = OpenAi::new("test-key", "https://api.openai.com/v1/", "test-model")
        .expect("provider")
        .with_reasoning_effort("medium")
        .expect("reasoning effort");

    assert_eq!(
        provider
            .response_body(model_request())
            .expect("response body")["reasoning"],
        serde_json::json!({"effort": "medium"})
    );
}

#[test]
fn compatible_endpoint_does_not_assume_reasoning_summary_support() {
    let provider = OpenAi::new("test-key", "https://example.com/v1", "test-model")
        .expect("provider")
        .with_reasoning_effort("medium")
        .expect("reasoning effort");

    assert_eq!(
        provider
            .response_body(model_request())
            .expect("response body")["reasoning"],
        serde_json::json!({"effort": "medium"})
    );
}

#[test]
fn compatible_endpoint_can_opt_into_automatic_reasoning_summaries() {
    let provider = OpenAi::new("test-key", "https://example.com/v1", "test-model")
        .expect("provider")
        .with_reasoning_effort("medium")
        .expect("reasoning effort")
        .with_reasoning_summary();

    assert_eq!(
        provider
            .response_body(model_request())
            .expect("response body")["reasoning"],
        serde_json::json!({"effort": "medium", "summary": "auto"})
    );
}

#[test]
fn reasoning_summary_opt_in_can_use_the_models_default_effort() {
    let provider = OpenAi::new("test-key", "https://example.com/v1", "test-model")
        .expect("provider")
        .with_reasoning_summary();

    assert_eq!(
        provider
            .response_body(model_request())
            .expect("response body")["reasoning"],
        serde_json::json!({"summary": "auto"})
    );
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
            "status": "completed",
            "summary": []
        }),
        serde_json::json!({
            "type": "function_call",
            "call_id": "call-1",
            "name": "inspect",
            "arguments": "{}",
            "status": "completed"
        }),
        serde_json::json!({
            "type": "message",
            "role": "assistant",
            "phase": "commentary",
            "status": "completed",
            "content": [{"type": "output_text", "text": "done"}]
        }),
        serde_json::json!({
            "type": "web_search_call",
            "status": "completed"
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
            serde_json::json!({
                "type": "function_call",
                "call_id": "call-1",
                "name": "inspect",
                "arguments": "{}"
            }),
            serde_json::json!({
                "type": "message",
                "role": "assistant",
                "phase": "commentary",
                "content": [{"type": "output_text", "text": "done"}]
            }),
            serde_json::json!({
                "type": "web_search_call",
                "status": "completed"
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
            "status": "completed",
            "summary": [{"type": "summary_text", "text": "Plan."}]
        }]
    }))
    .expect("decode response");

    assert_eq!(decoded.output()[0].get("format"), None);
    assert_eq!(decoded.output()[0].get("status"), None);
    assert_eq!(decoded.output()[0][REPLAY_REASONING_FIELD], "Plan.");
}

#[test]
fn compact_decode_normalizes_provider_wire_items() {
    let decoded = decode_compact_response(serde_json::json!({
        "output": [
            {
                "type": "message",
                "id": "message-1",
                "role": "user",
                "status": "completed",
                "content": [{"type": "input_text", "text": "inspect"}]
            },
            {
                "type": "reasoning",
                "format": "openai-responses-v1",
                "status": "completed",
                "encrypted_content": "reasoning"
            },
            {
                "type": "compaction_summary",
                "encrypted_content": "opaque"
            }
        ]
    }))
    .expect("decode compact response");

    assert_eq!(
        decoded.output(),
        &[
            serde_json::json!({
                "type": "message",
                "id": "message-1",
                "role": "user",
                "content": [{"type": "input_text", "text": "inspect"}]
            }),
            serde_json::json!({
                "type": "reasoning",
                "encrypted_content": "reasoning"
            }),
            serde_json::json!({
                "type": "compaction",
                "encrypted_content": "opaque"
            })
        ]
    );
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
fn responses_decode_preserves_text_part_boundaries_and_annotations() {
    let decoded = decode_response(serde_json::json!({
        "output": [{
            "type": "message",
            "role": "assistant",
            "content": [
                {
                    "type": "output_text",
                    "text": "Source one.",
                    "annotations": [
                        {
                            "type": "url_citation",
                            "url": "https://example.com",
                            "title": "Example",
                            "start_index": 0,
                            "end_index": 10
                        },
                        {
                            "type": "file_citation",
                            "file_id": "file-1",
                            "filename": "notes.txt",
                            "index": 2
                        }
                    ]
                },
                {"type": "refusal", "refusal": "unused boundary"},
                {
                    "type": "output_text",
                    "text": "Source two.",
                    "annotations": [
                        {
                            "type": "container_file_citation",
                            "container_id": "container-1",
                            "file_id": "file-2",
                            "filename": "report.pdf",
                            "start_index": 0,
                            "end_index": 10
                        },
                        {
                            "type": "file_path",
                            "file_id": "file-3",
                            "index": 4
                        }
                    ]
                }
            ]
        }]
    }))
    .expect("decode response");

    assert_eq!(
        serde_json::to_value(decoded.content()).expect("serialize normalized content"),
        serde_json::json!([
            {
                "output_index": 0,
                "part_index": 0,
                "phase": "final_answer",
                "text": "Source one.",
                "annotations": [
                    {
                        "type": "url_citation",
                        "url": "https://example.com",
                        "title": "Example",
                        "start_index": 0,
                        "end_index": 10
                    },
                    {
                        "type": "file_citation",
                        "file_id": "file-1",
                        "filename": "notes.txt",
                        "index": 2
                    }
                ]
            },
            {
                "output_index": 0,
                "part_index": 2,
                "phase": "final_answer",
                "text": "Source two.",
                "annotations": [
                    {
                        "type": "container_file_citation",
                        "container_id": "container-1",
                        "file_id": "file-2",
                        "filename": "report.pdf",
                        "start_index": 0,
                        "end_index": 10
                    },
                    {"type": "file_path", "file_id": "file-3", "index": 4}
                ]
            }
        ])
    );
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
fn responses_emits_reasoning_summary_deltas() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink_seen = Arc::clone(&seen);
    let events: ModelEventSink = Arc::new(move |event| {
        sink_seen.lock().expect("events lock").push(event);
        Ok(())
    });

    assert!(
        emit_reasoning_event(
            &serde_json::json!({
                "type": "response.reasoning_summary_text.delta",
                "delta": "**Checking the request**"
            }),
            &events,
        )
        .expect("reasoning summary event")
    );
    assert_eq!(
        *seen.lock().expect("events lock"),
        vec![ModelEvent::ReasoningDelta(
            "**Checking the request**".into()
        )]
    );
}

#[test]
fn responses_emits_commentary_text_deltas() {
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink_seen = Arc::clone(&seen);
    let events: ModelEventSink = Arc::new(move |event| {
        sink_seen.lock().expect("events lock").push(event);
        Ok(())
    });
    let mut commentary = BTreeSet::new();

    emit_text_event(
        &serde_json::json!({
            "type": "response.output_item.added",
            "item": {
                "id": "message-1",
                "type": "message",
                "phase": "commentary"
            }
        }),
        &mut commentary,
        &events,
    )
    .expect("commentary item");
    emit_text_event(
        &serde_json::json!({
            "type": "response.output_text.delta",
            "item_id": "message-1",
            "delta": "Checking."
        }),
        &mut commentary,
        &events,
    )
    .expect("commentary delta");

    assert_eq!(
        *seen.lock().expect("events lock"),
        vec![ModelEvent::CommentaryDelta("Checking.".into())]
    );
}

#[test]
fn responses_web_search_preserves_every_query() {
    let action = decode_web_action(&serde_json::json!({
        "action": {
            "type": "search",
            "queries": ["Horus framework", "Horus gateway"]
        }
    }));

    assert_eq!(
        action,
        WebSearchAction::Search {
            queries: vec!["Horus framework".into(), "Horus gateway".into()]
        }
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
