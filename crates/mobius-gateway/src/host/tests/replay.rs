use super::*;

#[test]
fn replay_is_bounded_by_event_count() {
    let frame = ServerFrame::new(ServerMessage::Error {
        code: "test".into(),
        message: String::new(),
        fatal: false,
    });
    let mut replay = VecDeque::from(vec![frame.clone(); REPLAY_CAPACITY]);
    let mut replay_bytes = serde_json::to_vec(&frame).expect("encode frame").len() * replay.len();
    let (events, _) = broadcast::channel(1);
    assert!(
        record_and_publish(&mut replay, &mut replay_bytes, &events, frame, true)
            .expect("record event")
    );
    assert_eq!(replay.len(), REPLAY_CAPACITY);
}

#[test]
fn replay_is_bounded_by_encoded_bytes() {
    let (events, _) = broadcast::channel(1);
    let mut replay = VecDeque::new();
    let mut replay_bytes = 0;
    let large_message = "x".repeat(MAX_REPLAY_BYTES / 2);
    let first = ServerFrame::new(ServerMessage::Error {
        code: "first".into(),
        message: large_message.clone(),
        fatal: false,
    });
    let second = ServerFrame::new(ServerMessage::Error {
        code: "second".into(),
        message: large_message,
        fatal: false,
    });

    assert!(
        !record_and_publish(&mut replay, &mut replay_bytes, &events, first, true)
            .expect("record first frame")
    );
    assert!(
        record_and_publish(&mut replay, &mut replay_bytes, &events, second, true)
            .expect("record second frame")
    );

    assert_eq!(replay.len(), 1);
    assert!(replay_bytes <= MAX_REPLAY_BYTES);
}

#[test]
fn suppressed_frames_enter_replay_without_broadcasting() {
    let mut replay = VecDeque::new();
    let mut replay_bytes = 0;
    let (events, mut receiver) = broadcast::channel(4);
    let history = ServerFrame::new(ServerMessage::Error {
        code: "history".into(),
        message: "recorded only".into(),
        fatal: false,
    });
    record_and_publish(
        &mut replay,
        &mut replay_bytes,
        &events,
        history.clone(),
        true,
    )
    .expect("record history");

    assert_eq!(replay.back(), Some(&history));
    assert!(matches!(
        receiver.try_recv(),
        Err(broadcast::error::TryRecvError::Empty)
    ));
}

#[test]
fn transient_controls_are_broadcast_without_entering_replay() {
    let (events, mut receiver) = broadcast::channel(5);
    let mut replay = VecDeque::new();
    let mut replay_bytes = 0;
    let messages = [
        EventMsg::SessionResumeRequested(mobius::protocol::SessionResumeRequestedEvent {
            session_id: "target".into(),
            context: Default::default(),
        }),
        EventMsg::Frontend(FrontendEvent::Preview {
            id: "preview".into(),
            title: "Preview".into(),
            subtitle: String::new(),
            page_id: "preview:latest".into(),
            update: mobius::protocol::FrontendPreviewUpdate::Replace,
            events: Vec::new(),
            next: None,
        }),
        EventMsg::Frontend(FrontendEvent::Picker {
            title: "Choose".into(),
            options: Vec::new(),
        }),
        EventMsg::Frontend(FrontendEvent::Widget {
            capability: "test".into(),
            item: mobius::protocol::FrontendWidget {
                id: "status".into(),
                slot: mobius::protocol::FrontendSlot::Header,
                text: "Current".into(),
                tone: mobius::protocol::FrontendTone::Neutral,
                symbol: None,
                icon_only: false,
                progress: None,
                content: None,
                action: None,
            },
        }),
        EventMsg::Frontend(FrontendEvent::RemoveWidget {
            capability: "test".into(),
            id: "status".into(),
        }),
    ];
    for (index, msg) in messages.into_iter().enumerate() {
        let frame = ServerFrame::new(ServerMessage::AgentEvent {
            session_id: "source".into(),
            record: RecordedEvent {
                sequence: u64::try_from(index + 1).expect("sequence"),
                recorded_at_ms: 1,
                event: Event {
                    submission_id: Some("transient".into()),
                    msg,
                },
                stream_metrics: Vec::new(),
                blocks: Vec::new(),
                preview: None,
            },
        });
        record_and_publish(
            &mut replay,
            &mut replay_bytes,
            &events,
            frame.clone(),
            false,
        )
        .expect("broadcast transient control");
        assert_eq!(receiver.try_recv().expect("live transient control"), frame);
    }

    assert!(replay.is_empty());
    assert_eq!(replay_bytes, 0);
}

#[test]
fn completed_step_compacts_only_its_progressive_replay_frames() {
    let frame = |sequence, msg| {
        ServerFrame::new(ServerMessage::AgentEvent {
            session_id: "session".into(),
            record: RecordedEvent {
                sequence,
                recorded_at_ms: 1,
                event: Event {
                    submission_id: Some("submission".into()),
                    msg,
                },
                stream_metrics: Vec::new(),
                blocks: Vec::new(),
                preview: None,
            },
        })
    };
    let mut replay = VecDeque::from([
        frame(
            1,
            EventMsg::AgentMessageContentDelta(mobius::protocol::AgentMessageContentDeltaEvent {
                session_id: "session".into(),
                turn_id: "turn".into(),
                model_step_id: "completed".into(),
                delta: "answer".into(),
                phase: mobius::protocol::AgentMessagePhase::FinalAnswer,
            }),
        ),
        frame(
            2,
            EventMsg::AgentReasoningContentDelta(
                mobius::protocol::AgentReasoningContentDeltaEvent {
                    session_id: "session".into(),
                    turn_id: "turn".into(),
                    model_step_id: "completed".into(),
                    delta: "reasoning".into(),
                },
            ),
        ),
        frame(
            3,
            EventMsg::AgentMessageContentDelta(mobius::protocol::AgentMessageContentDeltaEvent {
                session_id: "session".into(),
                turn_id: "turn".into(),
                model_step_id: "active".into(),
                delta: "partial".into(),
                phase: mobius::protocol::AgentMessagePhase::FinalAnswer,
            }),
        ),
    ]);
    let mut replay_bytes = replay
        .iter()
        .map(|frame| serde_json::to_vec(frame).expect("encode frame").len())
        .sum();

    compact_replay_deltas(&mut replay, &mut replay_bytes, "completed")
        .expect("compact completed step");

    assert_eq!(replay.len(), 1);
    assert_eq!(replay.front().and_then(event_sequence), Some(3));
    assert_eq!(
        replay_bytes,
        serde_json::to_vec(replay.front().expect("remaining frame"))
            .expect("encode remaining frame")
            .len()
    );
}

#[test]
fn replacement_startup_is_published_only_after_ready() {
    let (events, mut receiver) = broadcast::channel(4);
    let ready = ServerFrame::new(ServerMessage::Error {
        code: "ready".into(),
        message: String::new(),
        fatal: false,
    });
    let startup = ServerFrame::new(ServerMessage::Error {
        code: "startup".into(),
        message: String::new(),
        fatal: false,
    });

    publish_ready_and_pending(&events, ready, vec![startup]);

    assert!(matches!(
        receiver.try_recv().expect("ready frame").message,
        ServerMessage::Error { code, .. } if code == "ready"
    ));
    assert!(matches!(
        receiver.try_recv().expect("startup frame").message,
        ServerMessage::Error { code, .. } if code == "startup"
    ));
}

#[test]
fn artifact_catalog_uses_block_identity_and_upserts_updates() {
    let mut artifacts = VecDeque::new();
    let mut block = FrontendBlock {
        id: Some("tools/turn-a/call-a".into()),
        group: Some("tools/turn-a".into()),
        update: FrontendBlockUpdate::Replace,
        state: FrontendBlockState::Complete,
        role: FrontendBlockRole::Artifact,
        title: "Code diff".into(),
        text: "first diff".into(),
        symbol: None,
        format: FrontendBlockFormat::UnifiedDiff,
        tone: mobius::protocol::FrontendTone::Success,
        files: Vec::new(),
    };
    upsert_artifact(
        &mut artifacts,
        "session-a",
        &RenderedBlock {
            capability: "tools".into(),
            block: block.clone(),
        },
    );
    block.text = "updated diff".into();

    upsert_artifact(
        &mut artifacts,
        "session-a",
        &RenderedBlock {
            capability: "tools".into(),
            block,
        },
    );

    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].id, "block:5:toolstools/turn-a/call-a");
    assert_eq!(artifacts[0].block.text, "updated diff");
}

#[test]
fn artifact_catalog_scopes_equal_block_ids_by_capability() {
    let mut artifacts = VecDeque::new();
    for capability in ["tools", "review"] {
        upsert_artifact(
            &mut artifacts,
            "session-a",
            &RenderedBlock {
                capability: capability.into(),
                block: FrontendBlock {
                    id: Some("result".into()),
                    group: None,
                    update: FrontendBlockUpdate::Replace,
                    state: FrontendBlockState::Complete,
                    role: FrontendBlockRole::Artifact,
                    title: capability.into(),
                    text: "diff".into(),
                    symbol: None,
                    format: FrontendBlockFormat::UnifiedDiff,
                    tone: mobius::protocol::FrontendTone::Success,
                    files: Vec::new(),
                },
            },
        );
    }

    assert_eq!(
        artifacts
            .iter()
            .map(|artifact| artifact.id.as_str())
            .collect::<Vec<_>>(),
        ["block:5:toolsresult", "block:6:reviewresult"]
    );
}

#[test]
fn artifact_catalog_uses_session_file_metadata() {
    let mut artifacts = VecDeque::new();
    let block = FrontendBlock {
        id: Some("artifacts/turn-a/call-a".into()),
        group: Some("artifacts/turn-a".into()),
        update: FrontendBlockUpdate::Replace,
        state: FrontendBlockState::Complete,
        role: FrontendBlockRole::Artifact,
        title: "Sent report.xlsx".into(),
        text: String::new(),
        symbol: None,
        format: FrontendBlockFormat::PlainText,
        tone: mobius::protocol::FrontendTone::Success,
        files: vec![mobius::protocol::SessionFileReference {
            id: "file-a".into(),
            name: "report.xlsx".into(),
            size: 42,
            media_type: "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet".into(),
        }],
    };

    upsert_artifact(
        &mut artifacts,
        "session-a",
        &RenderedBlock {
            capability: "artifacts".into(),
            block,
        },
    );

    assert_eq!(
        artifacts
            .front()
            .map(|artifact| (artifact.kind, artifact.title.as_str())),
        Some((ArtifactKind::File, "report.xlsx"))
    );
}

#[test]
fn stored_files_restore_the_artifact_catalog_without_live_replay() {
    let file = mobius::protocol::SessionFileReference {
        id: "file-a".into(),
        name: "report.xlsx".into(),
        size: 42,
        media_type: "application/octet-stream".into(),
    };

    let artifacts = merge_stored_file_artifacts(&VecDeque::new(), "session-a", vec![file.clone()]);

    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].session_id, "session-a");
    assert_eq!(artifacts[0].kind, ArtifactKind::File);
    assert_eq!(artifacts[0].title, "report.xlsx");
    assert_eq!(artifacts[0].block.files, [file]);
}
