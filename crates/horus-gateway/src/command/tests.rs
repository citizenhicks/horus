use super::*;

#[cfg(unix)]
#[test]
fn reset_gateway_state_removes_an_empty_directory() {
    let root = tempfile::tempdir().expect("state parent");
    let state = root.path().join("gateway");
    std::fs::create_dir(&state).expect("empty state");

    reset_gateway_state(state.clone()).expect("reset empty state");

    assert!(!state.exists());
}

#[cfg(unix)]
#[test]
fn reset_gateway_state_removes_incompatible_marked_state() {
    let root = tempfile::tempdir().expect("state parent");
    let state = root.path().join("gateway");
    std::fs::create_dir(&state).expect("gateway state");
    std::fs::write(state.join(STATE_MARKER_FILE), "version = 999\n").expect("incompatible marker");

    reset_gateway_state(state.clone()).expect("reset incompatible state");

    assert!(!state.exists());
}

#[cfg(unix)]
#[test]
fn reset_gateway_state_preserves_an_unrelated_nonempty_directory() {
    let root = tempfile::tempdir().expect("state parent");
    let state = root.path().join("not-horus");
    std::fs::create_dir(&state).expect("unrelated directory");
    let unrelated = state.join("keep.txt");
    std::fs::write(&unrelated, "keep").expect("unrelated file");

    let error = reset_gateway_state(state).expect_err("unrelated state must be refused");

    assert!(error.to_string().contains("refusing to reset") && unrelated.exists());
}

#[cfg(unix)]
#[test]
fn reset_gateway_state_refuses_a_symlinked_directory() {
    let root = tempfile::tempdir().expect("state parent");
    let real = root.path().join("real");
    let link = root.path().join("gateway");
    std::fs::create_dir(&real).expect("real directory");
    std::fs::write(real.join(STATE_MARKER_FILE), "version = 999\n").expect("gateway marker");
    std::os::unix::fs::symlink(&real, &link).expect("gateway symlink");

    reset_gateway_state(link).expect_err("symlinked state must be refused");

    assert!(real.exists());
}

#[test]
fn failed_auth_initialization_removes_only_the_new_gateway_state() {
    let root = tempfile::tempdir().expect("state parent");
    let state = root.path().join("gateway");
    let sibling = root.path().join("keep");
    std::fs::write(&sibling, "keep").expect("sibling state");
    let (store, _) =
        ConfigStore::initialize(state.clone(), DEFAULT_LISTEN, None).expect("gateway config");
    std::fs::create_dir(store.auth_path()).expect("conflicting auth path");

    initialize_auth(&store).expect_err("auth initialization must fail");

    assert_eq!((state.exists(), sibling.exists()), (false, true));
}

#[test]
fn default_initialization_enables_quick_cloudflare_and_loopback() {
    let directory = tempfile::tempdir().expect("gateway state parent");
    let state = directory.path().join("gateway");

    initialize(InitOptions {
        state_dir: state.clone(),
        listen: DEFAULT_LISTEN,
        tls: None,
        cloudflare: None,
    })
    .expect("initialize gateway");
    let (_, config) = ConfigStore::open(state).expect("open gateway config");

    assert_eq!(
        (config.cloudflare, config.listen),
        (Some(CloudflareConfig::Quick), DEFAULT_LISTEN)
    );
}

#[test]
fn cloudflare_local_client_uses_the_authenticated_loopback_endpoint() {
    let directory = tempfile::tempdir().expect("gateway state");
    let path = directory.path().join("auth.json");
    let (auth, _) = AuthStore::initialize(path).expect("initialize auth");
    let config = GatewayConfig::new_cloudflare(DEFAULT_LISTEN, CloudflareConfig::Quick)
        .expect("Cloudflare config");

    let (endpoint, token) = provision_cloudflare_local_client(&auth, &config)
        .expect("provision local client")
        .expect("Cloudflare local client");

    assert_eq!(endpoint.to_string(), "tcp://127.0.0.1:8741");
    assert!(auth.authenticate(&token).is_ok());
}

#[test]
fn pairing_setup_payload_formats_a_wss_endpoint() {
    let endpoint = "wss://horus.example.com".parse().expect("WSS endpoint");

    assert_eq!(
        pairing_setup_payload(&endpoint, "one-time-code"),
        "horus-pair:v1|wss://horus.example.com|one-time-code"
    );
}

#[test]
fn pairing_qr_contains_the_validated_endpoint_and_code() {
    let endpoint = "wss://horus.example.com".parse().expect("WSS endpoint");

    assert_eq!(
        pairing_setup_url(&endpoint, "one-time-code").as_str(),
        "horus://pair?endpoint=wss%3A%2F%2Fhorus.example.com&code=one-time-code"
    );
    assert!(
        !pairing_setup_qr(&endpoint, "one-time-code")
            .expect("pairing QR")
            .is_empty()
    );
}

#[test]
fn cloudflare_connection_advertises_public_and_local_endpoints_with_one_code() {
    let public_endpoint = "wss://horus.example.com".parse().expect("WSS endpoint");
    let local_endpoint = "tcp://127.0.0.1:8741".parse().expect("TCP endpoint");
    let mut output = Vec::new();

    write_connection(
        &mut output,
        &public_endpoint,
        Some(&local_endpoint),
        "one-time-code",
        false,
    )
    .expect("write connection");

    assert_eq!(
        String::from_utf8(output).expect("UTF-8 output"),
        "public endpoint: wss://horus.example.com\n\
             local endpoint: tcp://127.0.0.1:8741\n\
             one-time code: one-time-code\n\
             setup code: horus-pair:v1|wss://horus.example.com|one-time-code\n\
             copy the setup code into Horus\n\
             another terminal: horus pair wss://horus.example.com one-time-code\n\
             local terminal: horus pair tcp://127.0.0.1:8741 one-time-code\n"
    );
}

#[test]
fn parse_serve_accepts_an_explicit_state_directory() {
    let command = parse(vec![
        "serve".into(),
        "--state-dir".into(),
        "/tmp/horus".into(),
    ])
    .expect("parse serve");

    assert!(matches!(
        command,
        Command::Serve {
            state_dir,
            background: false,
        } if state_dir == std::path::Path::new("/tmp/horus")
    ));
}

#[test]
fn parse_connect_accepts_a_public_endpoint_and_state_directory() {
    let command = parse(vec![
        "connect".into(),
        "--endpoint".into(),
        "tls://gateway.example:443".into(),
        "--state-dir".into(),
        "/tmp/horus".into(),
    ])
    .expect("parse connect");

    assert!(matches!(
        command,
        Command::Connect(ConnectOptions { state_dir, endpoint: Some(endpoint) })
            if state_dir == std::path::Path::new("/tmp/horus")
                && endpoint.to_string() == "tls://gateway.example:443"
    ));
}

#[test]
fn connection_endpoint_requires_an_explicit_tls_hostname() {
    let certificate = tempfile::NamedTempFile::new().expect("certificate");
    let private_key = tempfile::NamedTempFile::new().expect("private key");
    let config = GatewayConfig::new(
        "0.0.0.0:8741".parse().expect("listen"),
        Some(TlsConfig {
            certificate: certificate.path().to_path_buf(),
            private_key: private_key.path().to_path_buf(),
        }),
    )
    .expect("TLS config");

    let error = connection_endpoint(&config, None).expect_err("endpoint must be explicit");

    assert!(error.to_string().contains("--endpoint tls://HOST:PORT"));
}

#[test]
fn cloudflare_connection_uses_the_configured_wss_endpoint() {
    let config = GatewayConfig::new_cloudflare(
        DEFAULT_LISTEN,
        CloudflareConfig::named("horus.example.com").expect("hostname"),
    )
    .expect("Cloudflare config");

    let endpoint = connection_endpoint(&config, None)
        .expect("Cloudflare endpoint")
        .expect("named endpoint");

    assert_eq!(endpoint.to_string(), "wss://horus.example.com");
    assert!(endpoint.is_websocket());
}

#[test]
fn quick_cloudflare_connection_waits_for_the_runtime_endpoint() {
    let config = GatewayConfig::new_cloudflare(DEFAULT_LISTEN, CloudflareConfig::Quick)
        .expect("Cloudflare config");

    let endpoint = connection_endpoint(&config, None).expect("Cloudflare endpoint");

    assert!(endpoint.is_none());
}

#[test]
fn parse_serve_accepts_background_with_an_explicit_state_directory() {
    let command = parse(vec![
        "serve".into(),
        "--background".into(),
        "--state-dir".into(),
        "/tmp/horus".into(),
    ])
    .expect("parse background serve");

    assert!(matches!(
        command,
        Command::Serve {
            state_dir,
            background: true,
        } if state_dir == std::path::Path::new("/tmp/horus")
    ));
}

#[test]
fn parse_serve_rejects_duplicate_background_flags() {
    let error = parse(vec![
        "serve".into(),
        "--background".into(),
        "--background".into(),
    ])
    .expect_err("duplicate background flag must fail");

    assert!(error.to_string().contains("usage:"));
}

#[test]
fn parse_init_uses_machine_state_without_a_workspace() {
    let command = parse(vec![
        "init".into(),
        "--state-dir".into(),
        "/tmp/horus".into(),
        "--listen".into(),
        "127.0.0.1:9000".into(),
    ])
    .expect("parse init");

    assert!(matches!(
        command,
        Command::Init(InitOptions { state_dir, listen, tls, cloudflare })
            if state_dir == std::path::Path::new("/tmp/horus")
                && listen == "127.0.0.1:9000".parse().expect("listen")
                && tls.is_none()
                && cloudflare.is_none()
    ));
}

#[cfg(unix)]
#[test]
fn parse_init_loads_a_private_cloudflare_token_without_debugging_it() {
    let token = tempfile::NamedTempFile::new().expect("token file");
    std::fs::write(token.path(), "secret-tunnel-token").expect("write token");
    token
        .as_file()
        .set_permissions(std::fs::Permissions::from_mode(0o600))
        .expect("secure token");
    let command = parse(vec![
        "init".into(),
        "--cloudflare-hostname".into(),
        "horus.example.com".into(),
        "--cloudflare-token-file".into(),
        token.path().into(),
    ])
    .expect("parse Cloudflare init");

    assert!(!format!("{command:?}").contains("secret-tunnel-token"));
}

#[test]
fn init_rejects_the_removed_workspace_flag() {
    let error = parse(vec![
        "init".into(),
        "--workspace".into(),
        "/tmp/workspace".into(),
    ])
    .expect_err("workspace flag must be rejected");

    assert!(error.to_string().contains("usage:"));
}

#[test]
fn parse_rejects_the_removed_status_command() {
    let error = parse(vec!["status".into()]).expect_err("status must be removed");

    assert!(error.to_string().contains("usage:"));
}

#[test]
fn parse_exit_accepts_an_explicit_state_directory() {
    let command = parse(vec![
        "exit".into(),
        "--state-dir".into(),
        "/tmp/horus".into(),
    ])
    .expect("parse exit");

    assert!(matches!(
        command,
        Command::Exit { state_dir } if state_dir == std::path::Path::new("/tmp/horus")
    ));
}

#[test]
fn process_record_rejects_an_invalid_pid() {
    let directory = tempfile::tempdir().expect("process record directory");
    let path = directory.path().join(PROCESS_FILE);
    std::fs::write(&path, r#"{"pid":0,"endpoint":null}"#).expect("write process record");

    let error = open_process_record(&path).expect_err("invalid PID must fail");

    assert!(error.to_string().contains("invalid gateway process record"));
}

#[test]
fn process_record_rejects_a_non_websocket_runtime_endpoint() {
    let directory = tempfile::tempdir().expect("process record directory");
    let path = directory.path().join(PROCESS_FILE);
    std::fs::write(&path, r#"{"pid":1,"endpoint":"tcp://127.0.0.1:8741"}"#)
        .expect("write process record");

    let error = open_process_record(&path).expect_err("plaintext endpoint must fail");

    assert!(error.to_string().contains("must use wss://"));
}

#[cfg(unix)]
#[test]
fn process_record_carries_the_quick_tunnel_endpoint() {
    let directory = tempfile::tempdir().expect("process record directory");
    let endpoint: Endpoint = "wss://bright-river.trycloudflare.com"
        .parse()
        .expect("endpoint");
    let guard =
        ProcessRecordGuard::create(directory.path(), Some(&endpoint)).expect("process record");
    let (record, _) = open_process_record(&guard.path)
        .expect("read process record")
        .expect("process record");

    assert_eq!(record.endpoint().expect("valid endpoint"), Some(endpoint));
}

#[cfg(unix)]
#[test]
fn running_connect_controls_quick_tunnel_over_loopback() {
    let directory = tempfile::tempdir().expect("gateway state parent");
    let state = directory.path().join("gateway");
    let (store, config) =
        ConfigStore::initialize_quick_cloudflare(state, DEFAULT_LISTEN).expect("gateway config");
    let public_endpoint: Endpoint = "wss://bright-river.trycloudflare.com"
        .parse()
        .expect("public endpoint");
    let _process = ProcessRecordGuard::create(store.state_dir(), Some(&public_endpoint))
        .expect("running process");

    let (client_endpoint, pairing_endpoint) = running_connection_endpoints(&store, &config, None)
        .expect("running connect")
        .expect("running gateway");

    assert_eq!(client_endpoint.to_string(), "tcp://127.0.0.1:8741");
    assert_eq!(pairing_endpoint, public_endpoint);
}

#[cfg(unix)]
#[tokio::test]
async fn running_gateway_issues_a_code_for_another_client() {
    let directory = tempfile::tempdir().expect("gateway state");
    let (server, grant) = GatewayServer::bootstrap(
        directory.path().join("gateway"),
        "127.0.0.1:0".parse().expect("listen address"),
    )
    .await
    .expect("bootstrap gateway");
    let endpoint: Endpoint = format!("tcp://{}", server.listen_addr())
        .parse()
        .expect("gateway endpoint");
    let (shutdown, signal) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(server.serve_until(async move {
        let _ = signal.await;
    }));
    let (_first, identity) = GatewayClient::pair(&endpoint, grant.code, "first", ClientKind::Cli)
        .await
        .expect("pair first client");

    let code = request_running_pairing_code(&endpoint, &identity.token)
        .await
        .expect("request another code");
    let (_second, _) = GatewayClient::pair(&endpoint, code, "second", ClientKind::Ios)
        .await
        .expect("pair second client");

    assert!(!serving.is_finished());
    shutdown.send(()).expect("stop gateway");
    serving.await.expect("gateway task").expect("stop gateway");
}

#[cfg(unix)]
#[test]
fn startup_cleanup_removes_only_an_unlocked_process_record() {
    let directory = tempfile::tempdir().expect("process record directory");
    let path = directory.path().join(PROCESS_FILE);
    std::fs::write(&path, b"{").expect("partial process record");

    remove_unlocked_process_record(&path);

    assert!(!path.exists());
    let guard = ProcessRecordGuard::create(directory.path(), None).expect("locked process record");
    remove_unlocked_process_record(&path);
    assert!(path.exists());
    drop(guard);
}

#[cfg(unix)]
#[test]
fn process_record_lock_tracks_the_gateway_lifetime() {
    let directory = tempfile::tempdir().expect("process record directory");
    let guard = ProcessRecordGuard::create(directory.path(), None).expect("process record");
    let (_, file) = open_process_record(&guard.path)
        .expect("read process record")
        .expect("process record");

    assert!(process_is_running(&file).expect("locked process record"));
    assert_eq!(
        running_process_pid(&guard.path).expect("running process ID"),
        Some(std::process::id())
    );
    drop(guard);
    assert!(!directory.path().join(PROCESS_FILE).exists());
}

#[cfg(unix)]
#[test]
fn startup_lock_allows_only_one_lifecycle_operation() {
    let directory = tempfile::tempdir().expect("startup directory");
    let guard = StartupGuard::create(directory.path()).expect("startup lock");

    let error = StartupGuard::create(directory.path()).expect_err("competing startup");

    assert!(error.to_string().contains("already in progress"));
    drop(guard);
    StartupGuard::create(directory.path()).expect("released startup lock");
}

#[test]
fn parse_init_requires_both_tls_paths() {
    let error = parse(vec![
        "init".into(),
        "--tls-cert".into(),
        "/tmp/certificate.pem".into(),
    ])
    .expect_err("partial TLS config must fail");

    assert!(error.to_string().contains("supplied together"));
}
