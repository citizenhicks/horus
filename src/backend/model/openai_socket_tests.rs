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

struct RefreshingAuthorization {
    token: Mutex<String>,
    authorizations: Mutex<Vec<String>>,
    refreshes: std::sync::atomic::AtomicUsize,
}

impl RefreshingAuthorization {
    fn new() -> Self {
        Self {
            token: Mutex::new("rejected-token".into()),
            authorizations: Mutex::new(Vec::new()),
            refreshes: std::sync::atomic::AtomicUsize::new(0),
        }
    }

    fn resolve(&self) -> BoxFuture<'_, Result<ResolvedAuthorization>> {
        Box::pin(async move {
            let token = self.token.lock().await.clone();
            self.authorizations.lock().await.push(token.clone());
            Ok(ResolvedAuthorization {
                token,
                headers: Vec::new(),
            })
        })
    }
}

impl OpenAiAuthorization for RefreshingAuthorization {
    fn authorize_http<'a>(
        &'a self,
        _streaming: bool,
    ) -> BoxFuture<'a, Result<ResolvedAuthorization>> {
        self.resolve()
    }

    fn authorize_websocket<'a>(
        &'a self,
        _session_id: &'a str,
    ) -> BoxFuture<'a, Result<ResolvedAuthorization>> {
        self.resolve()
    }

    fn recover_unauthorized<'a>(&'a self, rejected_token: &'a str) -> BoxFuture<'a, Result<bool>> {
        Box::pin(async move {
            let mut token = self.token.lock().await;
            if token.as_str() == rejected_token {
                *token = "fresh-token".into();
                self.refreshes
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
            Ok(true)
        })
    }
}

#[tokio::test]
async fn websocket_unauthorized_refreshes_and_retries_once() {
    use tokio::io::AsyncReadExt as _;
    use tokio::io::AsyncWriteExt as _;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("WebSocket listener");
    let address = listener.local_addr().expect("WebSocket address");
    let server = tokio::spawn(async move {
        let (mut rejected, _) = listener.accept().await.expect("rejected connection");
        let mut request = Vec::new();
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let mut chunk = [0; 1_024];
            let count = rejected.read(&mut chunk).await.expect("handshake request");
            assert_ne!(count, 0, "handshake ended before its headers");
            request.extend_from_slice(&chunk[..count]);
        }
        assert!(
            String::from_utf8_lossy(&request).contains("Bearer rejected-token"),
            "first handshake should use the rejected token"
        );
        rejected
            .write_all(
                b"HTTP/1.1 401 Unauthorized\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .expect("unauthorized response");

        let (accepted, _) = listener.accept().await.expect("retried connection");
        tokio_tungstenite::accept_async(accepted)
            .await
            .expect("retried WebSocket handshake")
    });

    let auth = RefreshingAuthorization::new();
    let socket_url = format!("ws://{address}/responses");
    let socket = connect(&auth, &socket_url, "session-1")
        .await
        .expect("connection should recover");
    drop(socket);
    drop(server.await.expect("WebSocket server"));

    assert_eq!(
        auth.authorizations.lock().await.as_slice(),
        ["rejected-token", "fresh-token"]
    );
    assert_eq!(auth.refreshes.load(std::sync::atomic::Ordering::Relaxed), 1);
}
