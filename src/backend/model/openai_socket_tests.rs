use super::*;

#[test]
fn continuation_sends_only_new_items_and_resets_on_rewrite() {
    let known = vec![
        serde_json::json!({"role": "user", "content": "one"}),
        serde_json::json!({"role": "assistant", "content": "two"}),
    ];
    let mut state = SocketState {
        socket: None,
        continuation: Some(Continuation {
            response_id: "resp-1".into(),
            known_items: known.len(),
            fingerprint: fingerprint(known.iter()).expect("fingerprint"),
        }),
    };
    let mut continued = known.clone();
    continued.push(serde_json::json!({"type": "function_call_output"}));
    let (response, input) = continuation_input(&mut state, &continued).expect("continue");
    assert_eq!(response.as_deref(), Some("resp-1"));
    assert_eq!(
        input,
        &[serde_json::json!({"type": "function_call_output"})]
    );

    let rewritten = vec![serde_json::json!({"type": "compaction"})];
    let (response, input) = continuation_input(&mut state, &rewritten).expect("reset");
    assert_eq!(response, None);
    assert_eq!(input, rewritten);
    assert!(state.continuation.is_none());

    state.continuation = Some(Continuation {
        response_id: "resp-2".into(),
        known_items: known.len(),
        fingerprint: fingerprint(known.iter()).expect("fingerprint"),
    });
    let (response, input) = response_input(&mut state, &known, false).expect("stateless request");
    assert_eq!(response, None);
    assert_eq!(input, known);
    assert!(state.continuation.is_none());
}

#[tokio::test]
async fn active_socket_sessions_are_never_evicted() {
    let provider = OpenAiSocket::new("test-key", "test-model").expect("provider");
    let mut active = Vec::new();
    for index in 0..MAX_SOCKET_SESSIONS {
        active.push(
            provider
                .session(&format!("session-{index:03}"))
                .await
                .expect("session"),
        );
    }

    assert!(provider.session("overflow").await.is_err());
    drop(active.remove(0));
    provider
        .session("overflow")
        .await
        .expect("idle session can be evicted");

    let sessions = provider.sessions.lock().await;
    assert_eq!(sessions.len(), MAX_SOCKET_SESSIONS);
    assert!(sessions.contains_key("overflow"));
    assert!(
        active
            .iter()
            .all(|session| sessions.values().any(|cached| Arc::ptr_eq(cached, session)))
    );
}
