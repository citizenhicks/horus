use super::super::*;
use super::support::model_request;
use crate::backend::model::STREAM_RETRY_LIMIT;

#[test]
fn implicit_prompt_cache_omits_options() {
    let provider = OpenAiSocket::with_authorization(
        Arc::new(ApiKeyAuthorization::new("test-key".into())),
        "https://example.com/v1",
        "wss://example.com/v1/responses",
        "test-model",
        reqwest::Client::new(),
    )
    .expect("provider");
    let body = response_body(
        "test-model",
        &model_request(),
        &[],
        None,
        None,
        &[],
        provider.explicit_prompt_cache,
    )
    .expect("response body");

    assert_eq!(
        (
            provider.prompt_cache_capability(),
            body.get("prompt_cache_options")
        ),
        (PromptCacheCapability::Implicit, None)
    );
}

#[test]
fn generic_processing_error_is_a_retryable_stream_failure() {
    let request_id = "922d2b28-14a7-4b76-be1e-ae6be18309b9";
    let event = serde_json::json!({
        "type": "response.failed",
        "response": {
            "error": {
                "message": format!(
                    "An error occurred while processing your request. You can retry your request. Please include the request ID {request_id} in your message."
                )
            }
        }
    });

    let Exchange::Retry { retry_after } =
        failed_exchange(&event, false).expect("retryable failure")
    else {
        panic!("expected retryable exchange");
    };
    assert_eq!(retry_after, None);
    assert!(matches!(
        failed_exchange(&event, true).expect("streamed failure"),
        Exchange::Retry { retry_after: None }
    ));
}

#[test]
fn stream_failures_do_not_enable_http_fallback() {
    let mut state = SocketState {
        connection: None,
        continuation: None,
        use_http: false,
        last_used_at: Instant::now(),
    };

    for _ in 0..STREAM_RETRY_LIMIT {
        let Error::Provider(error) = websocket_failure(&mut state, None) else {
            panic!("expected provider error");
        };
        assert!(error.is_stream_interrupted());
    }

    assert!(!state.use_http);
}

#[test]
fn continuation_sends_only_new_items_and_resets_on_rewrite() {
    let known = vec![
        serde_json::json!({"role": "user", "content": "one"}),
        serde_json::json!({"role": "assistant", "content": "two"}),
    ];
    let mut state = SocketState {
        connection: None,
        continuation: Some(Continuation {
            response_id: "resp-1".into(),
            known_items: known.len(),
            fingerprint: fingerprint(known.iter()).expect("fingerprint"),
            envelope_fingerprint: envelope_fingerprint("test-model", &model_request(), None, &[])
                .expect("envelope fingerprint"),
        }),
        use_http: false,
        last_used_at: Instant::now(),
    };
    let mut continued = known.clone();
    continued.push(serde_json::json!({"type": "function_call_output"}));
    let envelope = envelope_fingerprint("test-model", &model_request(), None, &[])
        .expect("envelope fingerprint");
    let (response, input) = continuation_input(&mut state, &continued, envelope).expect("continue");
    assert_eq!(response.as_deref(), Some("resp-1"));
    assert_eq!(
        input,
        &[serde_json::json!({"type": "function_call_output"})]
    );

    let changed_envelope = envelope_fingerprint(
        "test-model",
        &ModelRequest {
            instructions: "Changed instructions",
            ..model_request()
        },
        None,
        &[],
    )
    .expect("changed envelope fingerprint");
    let (response, input) =
        continuation_input(&mut state, &continued, changed_envelope).expect("envelope reset");
    assert_eq!(response, None);
    assert_eq!(input, continued);
    assert!(state.continuation.is_none());

    let rewritten = vec![serde_json::json!({"type": "compaction"})];
    let (response, input) = continuation_input(&mut state, &rewritten, envelope).expect("reset");
    assert_eq!(response, None);
    assert_eq!(input, rewritten);
    assert!(state.continuation.is_none());

    state.continuation = Some(Continuation {
        response_id: "resp-2".into(),
        known_items: known.len(),
        fingerprint: fingerprint(known.iter()).expect("fingerprint"),
        envelope_fingerprint: envelope,
    });
    let (response, input) =
        response_input(&mut state, &known, false, envelope).expect("stateless request");
    assert_eq!(response, None);
    assert_eq!(input, known);
    assert!(state.continuation.is_none());
}
