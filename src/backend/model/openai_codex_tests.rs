use super::*;

#[cfg(unix)]
#[test]
fn saved_auth_is_owner_only() {
    let directory = tempfile::tempdir().expect("temporary directory");
    let path = directory.path().join("auth.json");
    let credential = OAuthCredential {
        access: "access-token".into(),
        refresh: "refresh-token".into(),
        expires: u64::MAX,
        account_id: "account-123".into(),
    };

    write_credential(&path, &credential).expect("save credential");

    assert_eq!(
        fs::metadata(path)
            .expect("auth metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[tokio::test]
async fn callback_rejects_wrong_state_then_accepts_the_expected_state() {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("callback listener");
    let address = listener.local_addr().expect("callback address");
    let callback = tokio::spawn(wait_for_callback(listener, "expected"));

    let mut wrong = TcpStream::connect(address).await.expect("wrong callback");
    wrong
        .write_all(b"GET /auth/callback?code=wrong&state=other HTTP/1.1\r\nHost: localhost\r\n\r\n")
        .await
        .expect("write wrong callback");
    let mut response = Vec::new();
    wrong
        .read_to_end(&mut response)
        .await
        .expect("read wrong response");
    assert!(response.starts_with(b"HTTP/1.1 400"));

    let mut correct = TcpStream::connect(address).await.expect("correct callback");
    correct
        .write_all(
            b"GET /auth/callback?code=accepted&state=expected HTTP/1.1\r\nHost: localhost\r\n\r\n",
        )
        .await
        .expect("write correct callback");

    assert_eq!(
        callback
            .await
            .expect("callback task")
            .expect("callback result"),
        "accepted"
    );
}

#[tokio::test]
async fn callback_timeout_bounds_the_whole_connection() {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("callback listener");
    let address = listener.local_addr().expect("callback address");
    let client = tokio::spawn(async move {
        let mut stream = TcpStream::connect(address).await.expect("callback client");
        stream.write_all(b"G").await.expect("partial callback");
        std::future::pending::<()>().await;
    });
    let (mut stream, _) = listener.accept().await.expect("callback connection");

    let error = read_callback_request_with_timeout(&mut stream, Duration::from_millis(10))
        .await
        .expect_err("partial callback should time out");
    client.abort();

    assert_eq!(
        error.to_string(),
        "authentication error: OAuth callback request timed out"
    );
}

#[tokio::test]
async fn callback_limit_applies_before_the_header_delimiter() {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("callback listener");
    let address = listener.local_addr().expect("callback address");
    let client = tokio::spawn(async move {
        let mut stream = TcpStream::connect(address).await.expect("callback client");
        let mut request = vec![b'X'; CALLBACK_LIMIT];
        request.extend_from_slice(b"\r\n\r\n");
        stream
            .write_all(&request)
            .await
            .expect("oversized callback");
    });
    let (mut stream, _) = listener.accept().await.expect("callback connection");

    let error = read_callback_request(&mut stream)
        .await
        .expect_err("oversized callback should fail");
    client.await.expect("callback client");

    assert_eq!(
        error.to_string(),
        "authentication error: OAuth callback request was too large"
    );
}

#[test]
fn credentials_refresh_with_clock_skew_leeway() {
    let credential = OAuthCredential {
        access: "access-token".into(),
        refresh: "refresh-token".into(),
        expires: now() + REFRESH_LEEWAY.as_secs() - 1,
        account_id: "account-123".into(),
    };

    assert!(expired(&credential));
}
