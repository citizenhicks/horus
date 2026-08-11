use super::*;
use tokio::io::AsyncReadExt as _;
use tokio::io::AsyncWriteExt as _;
use tokio_tungstenite::connect_async;

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

fn completed_events(text: &str, response_id: &str) -> [Value; 2] {
    [
        serde_json::json!({
            "type": "response.output_item.done",
            "output_index": 0,
            "item": {
                "id": format!("message-{response_id}"),
                "type": "message",
                "role": "assistant",
                "content": [{"type": "output_text", "text": text}]
            }
        }),
        serde_json::json!({
            "type": "response.completed",
            "response": {"id": response_id, "output": []}
        }),
    ]
}

async fn read_http_json(stream: &mut tokio::net::TcpStream) -> Value {
    let mut request = Vec::new();
    let header_end = loop {
        if let Some(index) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
            break index + 4;
        }
        let mut chunk = [0; 4_096];
        let count = stream.read(&mut chunk).await.expect("HTTP request");
        assert_ne!(count, 0, "HTTP request ended before its headers");
        request.extend_from_slice(&chunk[..count]);
    };
    let headers = String::from_utf8_lossy(&request[..header_end]);
    let content_length = headers
        .lines()
        .find_map(|line| {
            line.split_once(':').and_then(|(name, value)| {
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().expect("content length"))
            })
        })
        .expect("content-length header");
    while request.len() < header_end + content_length {
        let mut chunk = [0; 4_096];
        let count = stream.read(&mut chunk).await.expect("HTTP request body");
        assert_ne!(count, 0, "HTTP request body ended early");
        request.extend_from_slice(&chunk[..count]);
    }
    serde_json::from_slice(&request[header_end..header_end + content_length])
        .expect("HTTP JSON body")
}

async fn write_http_stream(stream: &mut tokio::net::TcpStream, text: &str, response_id: &str) {
    let body = completed_events(text, response_id)
        .into_iter()
        .map(|event| format!("data: {event}\n\n"))
        .collect::<String>();
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream
        .write_all(response.as_bytes())
        .await
        .expect("HTTP stream response");
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

#[tokio::test]
async fn non_authentication_handshake_rejection_is_retryable() {
    use tokio::io::AsyncReadExt as _;
    use tokio::io::AsyncWriteExt as _;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("WebSocket listener");
    let address = listener.local_addr().expect("WebSocket address");
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("WebSocket connection");
        let mut request = Vec::new();
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let mut chunk = [0; 1_024];
            let count = stream.read(&mut chunk).await.expect("handshake request");
            assert_ne!(count, 0, "request ended before its headers");
            request.extend_from_slice(&chunk[..count]);
        }
        stream
            .write_all(
                b"HTTP/1.1 426 Upgrade Required\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .expect("handshake rejection");
    });
    let auth = ApiKeyAuthorization::new("test-key".into());

    let error = match connect(&auth, &format!("ws://{address}/responses"), "session").await {
        Ok(connection) => {
            connection.close().await;
            panic!("handshake unexpectedly succeeded");
        }
        Err(error) => error,
    };
    server.await.expect("WebSocket server");

    let Error::Provider(error) = error else {
        panic!("expected provider error");
    };
    assert!(error.is_stream_interrupted());
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
        }),
        websocket_failures: 0,
        use_http: false,
        last_used_at: Instant::now(),
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

#[tokio::test]
async fn idle_socket_sessions_expire_before_new_sessions_are_cached() {
    let provider = OpenAiSocket::new("test-key", "test-model").expect("provider");
    let stale = provider.session("stale").await.expect("stale session");
    stale.lock().await.last_used_at = Instant::now() - CONNECTION_IDLE_TIMEOUT;
    drop(stale);

    provider.session("fresh").await.expect("fresh session");

    let sessions = provider.sessions.lock().await;
    assert!(!sessions.contains_key("stale"));
    assert!(sessions.contains_key("fresh"));
}

#[tokio::test]
async fn sticky_http_sessions_do_not_consume_websocket_capacity() {
    let provider = OpenAiSocket::new("test-key", "test-model").expect("provider");
    for index in 0..MAX_SOCKET_SESSIONS {
        let session = provider
            .session(&format!("fallback-{index:03}"))
            .await
            .expect("fallback session");
        session.lock().await.use_http = true;
    }

    let overflow = provider
        .session("overflow")
        .await
        .expect("HTTP-only sessions do not hold provider connections");
    assert!(!overflow.lock().await.use_http);
    drop(overflow);

    let sessions = provider.sessions.lock().await;
    assert_eq!(sessions.len(), MAX_SOCKET_SESSIONS + 1);
    for index in 0..MAX_SOCKET_SESSIONS {
        let session = sessions
            .get(&format!("fallback-{index:03}"))
            .expect("sticky HTTP session retained");
        assert!(
            session
                .try_lock()
                .is_ok_and(|state| state.use_http && state.connection.is_none())
        );
    }
}

#[tokio::test]
async fn metadata_only_event_does_not_count_as_sink_delivery() {
    let (sender, mut messages) = mpsc::unbounded_channel();
    sender
        .send(SocketEvent::Message(Message::text(
            serde_json::json!({
                "type": "response.output_item.added",
                "item": {
                    "id": "commentary-1",
                    "type": "message",
                    "phase": "commentary"
                }
            })
            .to_string(),
        )))
        .expect("metadata event");
    drop(sender);
    let delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let sink_delivered = Arc::clone(&delivered);
    let events: ModelEventSink = Arc::new(move |_| {
        sink_delivered.fetch_add(1, Ordering::Relaxed);
        Ok(())
    });

    let exchange = read_exchange(&mut messages, &events)
        .await
        .expect("exchange result");

    assert!(matches!(exchange, Exchange::Reconnect));
    assert_eq!(delivered.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn visible_delta_before_eof_is_still_a_retryable_stream_failure() {
    let (sender, mut messages) = mpsc::unbounded_channel();
    sender
        .send(SocketEvent::Message(Message::text(
            serde_json::json!({
                "type": "response.output_text.delta",
                "delta": "partial"
            })
            .to_string(),
        )))
        .expect("text delta");
    drop(sender);
    let delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let sink_delivered = Arc::clone(&delivered);
    let events: ModelEventSink = Arc::new(move |_| {
        sink_delivered.fetch_add(1, Ordering::Relaxed);
        Ok(())
    });

    let exchange = read_exchange(&mut messages, &events)
        .await
        .expect("exchange result");

    assert!(matches!(exchange, Exchange::Reconnect));
    assert_eq!(delivered.load(Ordering::Relaxed), 1);
}

#[tokio::test]
async fn server_close_before_completion_is_retryable() {
    let (sender, mut messages) = mpsc::unbounded_channel();
    sender.send(SocketEvent::Closed).expect("server close");
    let events: ModelEventSink = Arc::new(|_| Ok(()));

    let exchange = read_exchange(&mut messages, &events)
        .await
        .expect("exchange result");

    assert!(matches!(exchange, Exchange::Reconnect));
}

#[tokio::test]
async fn completed_tool_call_before_eof_is_not_returned() {
    let (sender, mut messages) = mpsc::unbounded_channel();
    sender
        .send(SocketEvent::Message(Message::text(
            serde_json::json!({
                "type": "response.output_item.done",
                "output_index": 0,
                "item": {
                    "type": "function_call",
                    "id": "item-1",
                    "call_id": "call-1",
                    "name": "dangerous_tool",
                    "arguments": "{}"
                }
            })
            .to_string(),
        )))
        .expect("tool call item");
    drop(sender);
    let delivered = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let sink_delivered = Arc::clone(&delivered);
    let events: ModelEventSink = Arc::new(move |_| {
        sink_delivered.fetch_add(1, Ordering::Relaxed);
        Ok(())
    });

    let exchange = read_exchange(&mut messages, &events)
        .await
        .expect("exchange result");

    assert!(matches!(exchange, Exchange::Reconnect));
    assert_eq!(delivered.load(Ordering::Relaxed), 0);
}

#[tokio::test(start_paused = true)]
async fn stream_idle_timeout_is_retryable() {
    let (_sender, mut messages) = mpsc::unbounded_channel();
    let events: ModelEventSink = Arc::new(|_| Ok(()));
    let exchange = tokio::spawn(async move { read_exchange(&mut messages, &events).await });
    tokio::task::yield_now().await;

    tokio::time::advance(STREAM_IDLE_TIMEOUT).await;
    let exchange = exchange
        .await
        .expect("exchange task")
        .expect("exchange result");

    assert!(matches!(exchange, Exchange::Reconnect));
}

#[tokio::test]
async fn idle_connection_pump_answers_ping() {
    use futures_util::SinkExt as _;
    use futures_util::StreamExt as _;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("WebSocket listener");
    let address = listener.local_addr().expect("WebSocket address");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("WebSocket connection");
        let mut socket = tokio_tungstenite::accept_async(stream)
            .await
            .expect("WebSocket handshake");
        socket
            .send(Message::Ping(vec![1, 2, 3].into()))
            .await
            .expect("ping");
        loop {
            match socket
                .next()
                .await
                .expect("pong frame")
                .expect("valid frame")
            {
                Message::Pong(payload) => break payload,
                Message::Ping(_) | Message::Text(_) | Message::Binary(_) | Message::Frame(_) => {}
                Message::Close(_) => panic!("connection closed before pong"),
            }
        }
    });
    let (socket, _) = connect_async(format!("ws://{address}"))
        .await
        .expect("client connection");
    let connection = OpenAiWsConnection::new(socket);

    let pong = timeout(Duration::from_secs(1), server)
        .await
        .expect("idle pong timed out")
        .expect("WebSocket server");
    connection.close().await;

    assert_eq!(pong.as_ref(), [1, 2, 3]);
}

#[tokio::test]
async fn active_connection_pump_forwards_bursts_without_blocking_ping() {
    use futures_util::SinkExt as _;
    use futures_util::StreamExt as _;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("WebSocket listener");
    let address = listener.local_addr().expect("WebSocket address");
    let message_count = 96;
    let (pong_sender, pong_received) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("WebSocket connection");
        let mut socket = tokio_tungstenite::accept_async(stream)
            .await
            .expect("WebSocket handshake");
        socket
            .next()
            .await
            .expect("response request")
            .expect("valid response request");
        for index in 0..message_count {
            socket
                .send(Message::text(index.to_string()))
                .await
                .expect("stream message");
        }
        socket
            .send(Message::Ping(vec![1, 2, 3].into()))
            .await
            .expect("active ping");
        loop {
            match socket
                .next()
                .await
                .expect("pong frame")
                .expect("valid pong frame")
            {
                Message::Pong(payload) => {
                    pong_sender.send(payload).expect("report pong");
                    break;
                }
                Message::Ping(_) | Message::Text(_) | Message::Binary(_) | Message::Frame(_) => {}
                Message::Close(_) => panic!("connection closed before pong"),
            }
        }
        let _ = socket.next().await;
    });
    let (socket, _) = connect_async(format!("ws://{address}"))
        .await
        .expect("client connection");
    let mut connection = OpenAiWsConnection::new(socket);
    connection
        .start(Message::text("request"))
        .await
        .expect("start exchange");
    tokio::time::sleep(Duration::from_millis(25)).await;
    let pong = timeout(Duration::from_secs(1), pong_received)
        .await
        .expect("active pong timed out")
        .expect("pong sender");

    for index in 0..message_count {
        let event = timeout(Duration::from_secs(1), connection.messages.recv())
            .await
            .expect("stream message timed out")
            .expect("stream remained open");
        let SocketEvent::Message(Message::Text(text)) = event else {
            panic!("unexpected socket event");
        };
        assert_eq!(text.as_str(), index.to_string());
    }
    assert!(!connection.closed.load(Ordering::Acquire));
    assert_eq!(pong.as_ref(), [1, 2, 3]);

    connection.finish();
    connection.close().await;
    server.await.expect("WebSocket server");
}

#[tokio::test]
async fn connection_age_limit_closes_before_another_send() {
    use futures_util::StreamExt as _;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("WebSocket listener");
    let address = listener.local_addr().expect("WebSocket address");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("WebSocket connection");
        let mut socket = tokio_tungstenite::accept_async(stream)
            .await
            .expect("WebSocket handshake");
        match timeout(Duration::from_secs(1), socket.next())
            .await
            .expect("connection age close")
        {
            Some(Ok(Message::Close(_))) | None => {}
            Some(Ok(message)) => panic!("expired socket received {message:?}"),
            Some(Err(_)) => {}
        }
    });
    let (socket, _) = connect_async(format!("ws://{address}"))
        .await
        .expect("client connection");
    let mut connection = OpenAiWsConnection::with_lifecycle(
        socket,
        Duration::from_millis(1),
        Duration::from_secs(60),
        Duration::from_secs(60),
    );

    timeout(Duration::from_secs(1), async {
        while !connection.closed.load(Ordering::Acquire) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("connection did not expire");

    assert!(!connection.is_usable());
    assert!(
        connection
            .start(Message::text("late request"))
            .await
            .is_err()
    );
    server.await.expect("WebSocket server");
}

#[tokio::test]
async fn active_response_survives_the_soft_reuse_age() {
    use futures_util::SinkExt as _;
    use futures_util::StreamExt as _;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("WebSocket listener");
    let address = listener.local_addr().expect("WebSocket address");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("WebSocket connection");
        let mut socket = tokio_tungstenite::accept_async(stream)
            .await
            .expect("WebSocket handshake");
        socket
            .next()
            .await
            .expect("response request")
            .expect("valid response request");
        tokio::time::sleep(Duration::from_millis(300)).await;
        socket
            .send(Message::text("response"))
            .await
            .expect("response message");
        let _ = socket.next().await;
    });
    let (socket, _) = connect_async(format!("ws://{address}"))
        .await
        .expect("client connection");
    let mut connection = OpenAiWsConnection::with_lifecycle(
        socket,
        Duration::from_millis(100),
        Duration::from_secs(2),
        Duration::from_secs(2),
    );
    connection
        .start(Message::text("request"))
        .await
        .expect("start exchange");

    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(!connection.closed.load(Ordering::Acquire));
    let message = timeout(Duration::from_secs(1), connection.messages.recv())
        .await
        .expect("response timed out")
        .expect("response event");

    assert!(matches!(message, SocketEvent::Message(Message::Text(_))));
    connection.finish();
    connection.close().await;
    server.await.expect("WebSocket server");
}

#[tokio::test]
async fn connection_limit_closes_other_idle_connections() {
    use futures_util::SinkExt as _;
    use futures_util::StreamExt as _;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("WebSocket listener");
    let address = listener.local_addr().expect("WebSocket address");
    let server = tokio::spawn(async move {
        let (idle_stream, _) = listener.accept().await.expect("idle connection");
        let mut idle_socket = tokio_tungstenite::accept_async(idle_stream)
            .await
            .expect("idle WebSocket handshake");

        let (limited_stream, _) = listener.accept().await.expect("limited connection");
        let mut limited_socket = tokio_tungstenite::accept_async(limited_stream)
            .await
            .expect("limited WebSocket handshake");
        limited_socket
            .next()
            .await
            .expect("response request")
            .expect("valid response request");
        limited_socket
            .send(Message::text(
                serde_json::json!({
                    "type": "error",
                    "error": {
                        "code": "websocket_connection_limit_reached",
                        "message": "connection limit reached"
                    }
                })
                .to_string(),
            ))
            .await
            .expect("connection-limit event");

        match timeout(Duration::from_secs(1), idle_socket.next())
            .await
            .expect("idle connection close")
        {
            Some(Ok(Message::Close(_))) | None => {}
            Some(Ok(message)) => panic!("idle socket received {message:?}"),
            Some(Err(_)) => {}
        }
    });
    let socket_url = format!("ws://{address}/responses");
    let provider = OpenAiSocket::with_authorization(
        Arc::new(ApiKeyAuthorization::new("test-key".into())),
        &format!("http://{address}"),
        &socket_url,
        "test-model",
        reqwest::Client::new(),
    )
    .expect("provider");
    let (idle_socket, _) = connect_async(&socket_url)
        .await
        .expect("idle client connection");
    let idle_session = provider
        .session("idle-session")
        .await
        .expect("idle session");
    idle_session.lock().await.connection = Some(OpenAiWsConnection::new(idle_socket));
    drop(idle_session);
    let events: ModelEventSink = Arc::new(|_| Ok(()));

    let Error::Provider(error) = provider
        .send_response(model_request(), events)
        .await
        .expect_err("connection limit should interrupt the attempt")
    else {
        panic!("expected provider error");
    };
    server.await.expect("WebSocket server");

    assert!(error.is_stream_interrupted());
    let sessions = provider.sessions.lock().await;
    let idle = Arc::clone(sessions.get("idle-session").expect("idle session retained"));
    drop(sessions);
    assert!(idle.lock().await.connection.is_none());
}

#[tokio::test]
async fn transient_response_failure_leaves_retry_to_a_fresh_model_attempt() {
    use futures_util::SinkExt as _;
    use futures_util::StreamExt as _;
    use tokio_tungstenite::tungstenite::Message;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("WebSocket listener");
    let address = listener.local_addr().expect("WebSocket address");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("WebSocket connection");
        let mut socket = tokio_tungstenite::accept_async(stream)
            .await
            .expect("WebSocket handshake");

        for attempt in 0..2 {
            let message = socket
                .next()
                .await
                .expect("response request")
                .expect("valid response request");
            let body: Value =
                serde_json::from_slice(&message.into_data()).expect("response request body");
            assert_eq!(body["type"], "response.create");

            if attempt == 0 {
                socket
                    .send(Message::text(
                        serde_json::json!({
                            "type": "response.failed",
                            "response": {
                                "error": {
                                    "type": "server_error",
                                    "message": "An error occurred while processing your request. You can retry your request, or contact support. Please include the request ID request-123 in your message."
                                }
                            }
                        })
                        .to_string(),
                    ))
                    .await
                    .expect("transient failure");
                continue;
            }

            socket
                .send(Message::text(
                    serde_json::json!({
                        "type": "response.output_item.done",
                        "output_index": 0,
                        "item": {
                            "id": "message-1",
                            "type": "message",
                            "role": "assistant",
                            "content": [{"type": "output_text", "text": "Recovered."}]
                        }
                    })
                    .to_string(),
                ))
                .await
                .expect("completed output item");
            socket
                .send(Message::text(
                    serde_json::json!({
                        "type": "response.completed",
                        "response": {"id": "response-1", "output": []}
                    })
                    .to_string(),
                ))
                .await
                .expect("completed response");
        }
    });

    let socket_url = format!("ws://{address}/responses");
    let http_url = format!("http://{address}");
    let provider = OpenAiSocket::with_authorization(
        Arc::new(ApiKeyAuthorization::new("test-key".into())),
        &http_url,
        &socket_url,
        "test-model",
        reqwest::Client::new(),
    )
    .expect("provider");
    let events: ModelEventSink = Arc::new(|_| Ok(()));

    let Error::Provider(error) = provider
        .send_response(model_request(), Arc::clone(&events))
        .await
        .expect_err("first model attempt should be interrupted")
    else {
        panic!("expected provider error");
    };
    assert!(error.is_stream_interrupted());

    let output = provider
        .send_response(model_request(), events)
        .await
        .expect("fresh model attempt should recover");
    server.await.expect("WebSocket server");

    assert_eq!(output.text(), "Recovered.");
}

#[tokio::test]
async fn previous_response_not_found_does_not_repeat_full_context() {
    use futures_util::SinkExt as _;
    use futures_util::StreamExt as _;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("WebSocket listener");
    let address = listener.local_addr().expect("WebSocket address");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("WebSocket connection");
        let mut socket = tokio_tungstenite::accept_async(stream)
            .await
            .expect("WebSocket handshake");
        let request: Value = serde_json::from_slice(
            &socket
                .next()
                .await
                .expect("response request")
                .expect("valid response request")
                .into_data(),
        )
        .expect("response body");
        assert!(request.get("previous_response_id").is_none());
        socket
            .send(Message::text(
                serde_json::json!({
                    "type": "error",
                    "error": {
                        "code": "previous_response_not_found",
                        "message": "Previous response was not found"
                    }
                })
                .to_string(),
            ))
            .await
            .expect("missing previous response");
        assert!(
            timeout(Duration::from_millis(250), socket.next())
                .await
                .is_err(),
            "full-context request was repeated"
        );
    });
    let socket_url = format!("ws://{address}/responses");
    let provider = OpenAiSocket::with_authorization(
        Arc::new(ApiKeyAuthorization::new("test-key".into())),
        &format!("http://{address}"),
        &socket_url,
        "test-model",
        reqwest::Client::new(),
    )
    .expect("provider");
    let events: ModelEventSink = Arc::new(|_| Ok(()));

    let Error::Provider(error) = provider
        .send_response(model_request(), events)
        .await
        .expect_err("missing previous ID should interrupt the model attempt")
    else {
        panic!("expected provider error");
    };
    server.await.expect("WebSocket server");

    assert!(error.is_stream_interrupted());
}

#[tokio::test]
async fn previous_response_not_found_rebuilds_full_context_on_the_same_connection() {
    use futures_util::SinkExt as _;
    use futures_util::StreamExt as _;

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("WebSocket listener");
    let address = listener.local_addr().expect("WebSocket address");
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("WebSocket connection");
        let mut socket = tokio_tungstenite::accept_async(stream)
            .await
            .expect("WebSocket handshake");
        let initial: Value = serde_json::from_slice(
            &socket
                .next()
                .await
                .expect("initial request")
                .expect("valid initial request")
                .into_data(),
        )
        .expect("initial request body");
        assert!(initial.get("previous_response_id").is_none());
        for event in completed_events("Old response.", "response-old") {
            socket
                .send(Message::text(event.to_string()))
                .await
                .expect("initial completed event");
        }

        let first: Value = serde_json::from_slice(
            &socket
                .next()
                .await
                .expect("continued request")
                .expect("valid continued request")
                .into_data(),
        )
        .expect("continued request body");
        assert_eq!(first["previous_response_id"], "response-old");
        assert_eq!(
            first["input"].as_array().expect("incremental input").len(),
            1
        );
        socket
            .send(Message::text(
                serde_json::json!({
                    "type": "error",
                    "error": {
                        "code": "previous_response_not_found",
                        "message": "Previous response was not found"
                    }
                })
                .to_string(),
            ))
            .await
            .expect("missing previous response");

        let rebuilt: Value = serde_json::from_slice(
            &socket
                .next()
                .await
                .expect("rebuilt request")
                .expect("valid rebuilt request")
                .into_data(),
        )
        .expect("rebuilt request body");
        assert!(rebuilt.get("previous_response_id").is_none());
        assert_eq!(rebuilt["input"].as_array().expect("full input").len(), 3);
        for event in completed_events("Recovered.", "response-new") {
            socket
                .send(Message::text(event.to_string()))
                .await
                .expect("completed event");
        }
    });
    let socket_url = format!("ws://{address}/responses");
    let provider = OpenAiSocket::with_authorization(
        Arc::new(ApiKeyAuthorization::new("test-key".into())),
        &format!("http://{address}"),
        &socket_url,
        "test-model",
        reqwest::Client::new(),
    )
    .expect("provider");
    let initial_input = vec![serde_json::json!({"role": "user", "content": "one"})];
    let events: ModelEventSink = Arc::new(|_| Ok(()));
    let initial_output = provider
        .send_response(
            ModelRequest {
                session_id: "test-session",
                instructions: "Test instructions",
                input: &initial_input,
                tools: &[],
                allow_hosted_tools: false,
                allow_continuation: true,
            },
            Arc::clone(&events),
        )
        .await
        .expect("initial response");
    let mut input = initial_input;
    input.extend_from_slice(initial_output.output());
    input.push(serde_json::json!({"role": "user", "content": "two"}));

    let output = provider
        .send_response(
            ModelRequest {
                session_id: "test-session",
                instructions: "Test instructions",
                input: &input,
                tools: &[],
                allow_hosted_tools: false,
                allow_continuation: true,
            },
            events,
        )
        .await
        .expect("context rebuild should recover");
    server.await.expect("WebSocket server");

    assert_eq!(output.text(), "Recovered.");
}

#[tokio::test]
async fn five_websocket_failures_switch_only_that_session_to_sticky_http() {
    use futures_util::SinkExt as _;
    use futures_util::StreamExt as _;

    let websocket_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("WebSocket listener");
    let websocket_address = websocket_listener.local_addr().expect("WebSocket address");
    let websocket_server = tokio::spawn(async move {
        let (stream, _) = websocket_listener
            .accept()
            .await
            .expect("initial WebSocket connection");
        let mut socket = tokio_tungstenite::accept_async(stream)
            .await
            .expect("initial WebSocket handshake");
        let initial: Value = serde_json::from_slice(
            &socket
                .next()
                .await
                .expect("initial response request")
                .expect("valid initial response request")
                .into_data(),
        )
        .expect("initial response body");
        assert!(initial.get("previous_response_id").is_none());
        for event in completed_events("Warm response.", "response-warm") {
            socket
                .send(Message::text(event.to_string()))
                .await
                .expect("initial completed event");
        }
        let continued: Value = serde_json::from_slice(
            &socket
                .next()
                .await
                .expect("continued response request")
                .expect("valid continued response request")
                .into_data(),
        )
        .expect("continued response body");
        assert_eq!(continued["previous_response_id"], "response-warm");
        assert_eq!(
            continued["input"]
                .as_array()
                .expect("incremental input")
                .len(),
            1
        );
        drop(socket);

        for _ in 1..STREAM_RETRY_LIMIT {
            let (stream, _) = websocket_listener
                .accept()
                .await
                .expect("failed WebSocket connection");
            let mut socket = tokio_tungstenite::accept_async(stream)
                .await
                .expect("WebSocket handshake");
            socket
                .next()
                .await
                .expect("response request")
                .expect("valid response request");
        }

        let (stream, _) = websocket_listener
            .accept()
            .await
            .expect("other session connection");
        let mut socket = tokio_tungstenite::accept_async(stream)
            .await
            .expect("other session handshake");
        socket
            .next()
            .await
            .expect("other session request")
            .expect("valid other session request");
        for event in completed_events("Other session.", "response-other") {
            socket
                .send(Message::text(event.to_string()))
                .await
                .expect("other session completed event");
        }
    });

    let http_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("HTTP listener");
    let http_address = http_listener.local_addr().expect("HTTP address");
    let (request_sender, mut requests) = mpsc::channel(3);
    let http_server = tokio::spawn(async move {
        for attempt in 0..3 {
            let (mut stream, _) = http_listener.accept().await.expect("HTTP connection");
            let request = read_http_json(&mut stream).await;
            request_sender
                .send(request)
                .await
                .expect("captured request");
            if attempt < 2 {
                write_http_stream(
                    &mut stream,
                    if attempt == 0 {
                        "HTTP fallback."
                    } else {
                        "Still HTTP."
                    },
                    &format!("response-http-{attempt}"),
                )
                .await;
            }
        }
    });

    let socket_url = format!("ws://{websocket_address}/responses");
    let provider = OpenAiSocket::with_authorization(
        Arc::new(ApiKeyAuthorization::new("test-key".into())),
        &format!("http://{http_address}"),
        &socket_url,
        "gpt-5.6-sol",
        reqwest::Client::new(),
    )
    .expect("provider")
    .with_reasoning_effort("medium")
    .expect("reasoning effort")
    .with_cached_web_search();
    let input = vec![serde_json::json!({
        "role": "user",
        "content": [{"type": "input_text", "text": "hello"}]
    })];
    let events: ModelEventSink = Arc::new(|_| Ok(()));

    let warm = provider
        .send_response(
            ModelRequest {
                session_id: "fallback-session",
                instructions: "Test instructions",
                input: &input,
                tools: &[],
                allow_hosted_tools: true,
                allow_continuation: true,
            },
            Arc::clone(&events),
        )
        .await
        .expect("initial WebSocket response");
    let mut continued_input = input.clone();
    continued_input.extend(warm.output().iter().cloned());
    continued_input.push(serde_json::json!({
        "role": "user",
        "content": [{"type": "input_text", "text": "continue"}]
    }));

    for _ in 0..STREAM_RETRY_LIMIT {
        let Error::Provider(error) = provider
            .send_response(
                ModelRequest {
                    session_id: "fallback-session",
                    instructions: "Test instructions",
                    input: &continued_input,
                    tools: &[],
                    allow_hosted_tools: true,
                    allow_continuation: true,
                },
                Arc::clone(&events),
            )
            .await
            .expect_err("WebSocket attempt should be interrupted")
        else {
            panic!("expected provider error");
        };
        assert!(error.is_stream_interrupted());
        assert_eq!(error.to_string(), "model response stream was interrupted");
    }

    let fallback = provider
        .send_response(
            ModelRequest {
                session_id: "fallback-session",
                instructions: "Test instructions",
                input: &continued_input,
                tools: &[],
                allow_hosted_tools: true,
                allow_continuation: true,
            },
            Arc::clone(&events),
        )
        .await
        .expect("HTTP fallback");
    let sticky = provider
        .send_response(
            ModelRequest {
                session_id: "fallback-session",
                instructions: "Test instructions",
                input: &continued_input,
                tools: &[],
                allow_hosted_tools: true,
                allow_continuation: true,
            },
            Arc::clone(&events),
        )
        .await
        .expect("sticky HTTP fallback");
    let Error::Provider(http_error) = provider
        .send_response(
            ModelRequest {
                session_id: "fallback-session",
                instructions: "Test instructions",
                input: &continued_input,
                tools: &[],
                allow_hosted_tools: true,
                allow_continuation: true,
            },
            Arc::clone(&events),
        )
        .await
        .expect_err("HTTPS failure should be terminal after fallback")
    else {
        panic!("expected provider error");
    };
    let other = provider
        .send_response(
            ModelRequest {
                session_id: "other-session",
                instructions: "Test instructions",
                input: &continued_input,
                tools: &[],
                allow_hosted_tools: true,
                allow_continuation: true,
            },
            events,
        )
        .await
        .expect("other session WebSocket");

    let first_http = requests.recv().await.expect("first HTTP request");
    let second_http = requests.recv().await.expect("second HTTP request");
    let failed_http = requests.recv().await.expect("failed HTTP request");
    http_server.await.expect("HTTP server");
    websocket_server.await.expect("WebSocket server");

    assert_eq!(fallback.text(), "HTTP fallback.");
    assert_eq!(sticky.text(), "Still HTTP.");
    assert_eq!(http_error.status(), None);
    assert!(!http_error.is_stream_interrupted());
    assert_eq!(http_error.to_string(), "HTTPS fallback transport failed");
    assert_eq!(other.text(), "Other session.");
    for request in [first_http, second_http, failed_http] {
        assert!(request.get("previous_response_id").is_none());
        assert_eq!(
            request["input"].as_array().expect("full HTTP input").len(),
            continued_input.len()
        );
        assert_eq!(
            request["reasoning"],
            serde_json::json!({"effort": "medium", "summary": "auto"})
        );
        assert_eq!(
            request["tools"],
            serde_json::json!([{"type": "web_search", "external_web_access": false}])
        );
    }
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
