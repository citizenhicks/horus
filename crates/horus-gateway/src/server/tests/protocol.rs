use super::*;

#[test]
fn tls_loader_rejects_empty_pem_files() {
    let file = tempfile::NamedTempFile::new().expect("temporary PEM");

    let error = load_certificates(file.path()).expect_err("empty PEM must fail");

    assert!(error.to_string().contains("certificate file is empty"));
}

#[tokio::test]
async fn paired_client_creates_workspace_directory_and_receives_its_listing() {
    let root = tempfile::tempdir().expect("root");
    let parent = root.path().join("projects");
    fs::create_dir(&parent).expect("parent");
    let (server, grant) = GatewayServer::bootstrap(
        root.path().join("state"),
        std::net::SocketAddr::from(([127, 0, 0, 1], 0)),
    )
    .await
    .expect("bootstrap gateway");
    let listen = server.listen_addr();
    let (shutdown, signal) = tokio::sync::oneshot::channel();
    let serving = tokio::spawn(server.serve_until(async move {
        let _ = signal.await;
    }));
    let endpoint = format!("tcp://{listen}")
        .parse::<Endpoint>()
        .expect("endpoint");
    let (connection, _) =
        GatewayClient::pair(&endpoint, grant.code, "directory test", ClientKind::Ios)
            .await
            .expect("connect client");
    let (sender, mut events) = connection.into_parts();
    wait_gateway_ready(&mut events).await;

    let request_id = "create-directory".to_string();
    sender
        .send(ClientMessage::CreateWorkspaceDirectory {
            request_id: request_id.clone(),
            parent: parent.clone(),
            name: "new-project".into(),
        })
        .await
        .expect("create workspace directory");
    loop {
        match next_gateway_message(&mut events).await {
            ServerMessage::Directories {
                request_id: actual,
                listing,
            } if actual == request_id => {
                assert_eq!(
                    listing.path,
                    fs::canonicalize(parent.join("new-project")).expect("created path")
                );
                assert!(listing.entries.is_empty());
                break;
            }
            ServerMessage::Rejected {
                request_id: actual,
                code,
                message,
                ..
            } if actual == request_id => {
                panic!("directory creation rejected ({code}): {message}");
            }
            _ => {}
        }
    }
    assert!(parent.join("new-project").is_dir());

    shutdown.send(()).expect("stop gateway");
    serving.await.expect("gateway task").expect("gateway stop");
}

#[test]
fn directory_listing_is_sorted_and_excludes_files() {
    let root = tempfile::tempdir().expect("temporary directory");
    fs::create_dir(root.path().join("zeta")).expect("create directory");
    fs::create_dir(root.path().join("Alpha")).expect("create directory");
    fs::write(root.path().join("notes.txt"), b"not a folder").expect("create file");

    let listing = list_directories(root.path(), false).expect("list directories");

    assert_eq!(
        listing
            .entries
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["Alpha", "zeta"]
    );
}

#[test]
fn directory_listing_can_include_files() {
    let root = tempfile::tempdir().expect("temporary directory");
    fs::create_dir(root.path().join("tasks")).expect("create directory");
    fs::create_dir(root.path().join(".git")).expect("create Git metadata");
    fs::write(root.path().join("daily.md"), b"task").expect("create file");

    let listing = list_directories(root.path(), true).expect("list directory entries");

    assert_eq!(listing.entries.len(), 2);
    assert!(
        listing
            .entries
            .iter()
            .any(|entry| { entry.name == "tasks" && entry.is_directory })
    );
    assert!(
        listing
            .entries
            .iter()
            .any(|entry| { entry.name == "daily.md" && !entry.is_directory })
    );
}

#[test]
fn history_frame_bound_rejects_one_oversized_turn() {
    let frame = ServerFrame::new(ServerMessage::SessionHistory {
        request_id: "history".into(),
        session_id: "session".into(),
        records: vec![crate::wire::RecordedEvent {
            sequence: 1,
            recorded_at_ms: 1,
            event: Event {
                submission_id: None,
                msg: EventMsg::Warning(horus::protocol::WarningEvent {
                    message: "x".repeat(MAX_FRAME_BYTES),
                }),
            },
            stream_metrics: Vec::new(),
            blocks: Vec::new(),
            preview: None,
        }],
        next_before_sequence: None,
    });

    assert!(!encoded_frame_fits(&frame).expect("measure history frame"));
}

#[test]
fn artifact_catalog_keeps_the_newest_suffix_that_fits_one_frame() {
    let artifacts = (0..3)
        .map(|index| large_artifact(index, MAX_FRAME_BYTES / 3))
        .collect();

    let frame = bounded_artifacts_frame("artifacts".into(), "session".into(), artifacts)
        .expect("bound artifact catalog");

    assert!(encoded_frame_fits(&frame).expect("measure artifact frame"));
    let ServerMessage::Artifacts {
        artifacts,
        truncated,
        ..
    } = frame.message
    else {
        panic!("expected artifact catalog");
    };
    assert!(truncated);
    assert_eq!(
        artifacts
            .iter()
            .map(|artifact| artifact.id.as_str())
            .collect::<Vec<_>>(),
        ["artifact-1", "artifact-2"]
    );
}

#[test]
fn artifact_catalog_is_empty_when_its_newest_record_cannot_fit() {
    let artifacts = vec![large_artifact(0, 1), large_artifact(1, MAX_FRAME_BYTES)];

    let frame = bounded_artifacts_frame("artifacts".into(), "session".into(), artifacts)
        .expect("bound artifact catalog");

    assert!(encoded_frame_fits(&frame).expect("measure artifact frame"));
    assert!(matches!(
        frame.message,
        ServerMessage::Artifacts {
            artifacts,
            truncated: true,
            ..
        } if artifacts.is_empty()
    ));
}

fn large_artifact(index: usize, text_bytes: usize) -> ArtifactRecord {
    let id = format!("artifact-{index}");
    ArtifactRecord {
        id: id.clone(),
        session_id: "session".into(),
        kind: ArtifactKind::CodeDiff,
        title: id.clone(),
        block: FrontendBlock {
            id: Some(id),
            group: None,
            update: FrontendBlockUpdate::Replace,
            state: FrontendBlockState::Complete,
            role: FrontendBlockRole::Artifact,
            title: "Code diff".into(),
            text: "x".repeat(text_bytes),
            symbol: None,
            files: Vec::new(),
            format: FrontendBlockFormat::UnifiedDiff,
            tone: FrontendTone::Neutral,
        },
    }
}

#[tokio::test]
async fn rejection_frames_preserve_request_correlation() {
    let (mut writer, reader) = tokio::io::duplex(1024);
    let mut reader = FrameReader::new(reader);
    write_rejection(
        &mut writer,
        "request-7".into(),
        Rejection {
            code: "agent_busy",
            message: "busy".into(),
            fatal: false,
        },
    )
    .await
    .expect("write rejection");

    let frame = read_frame::<ServerFrame>(&mut reader)
        .await
        .expect("read rejection")
        .expect("frame");

    assert!(matches!(
        frame.message,
        ServerMessage::Rejected { request_id, .. } if request_id == "request-7"
    ));
}
