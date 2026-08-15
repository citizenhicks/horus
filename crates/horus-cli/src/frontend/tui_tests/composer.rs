use super::support::*;
use super::*;

#[test]
fn composer_dispatches_commands_and_active_turn_steering() {
    let catalog = default_catalog();
    let mut idle = state();
    idle.input = "/exit".into();
    idle.cursor = idle.input.len();

    assert_eq!(idle.submit_input(&catalog), UiAction::Exit);

    let mut working = state();
    working.active_turn = Some("turn".into());
    working.input = "change direction".into();
    working.cursor = working.input.len();

    assert_eq!(
        working.submit_input(&catalog),
        UiAction::Submit(Op::ActiveInput {
            operation: "steer".into(),
            turn_id: "turn".into(),
            text: "change direction".into(),
        })
    );
}

#[test]
fn new_and_clear_keep_distinct_terminal_semantics() {
    let catalog = default_catalog();
    let mut new = state();
    new.input = "/new".into();
    new.cursor = new.input.len();
    let mut clear = state();
    clear.input = "/clear".into();
    clear.cursor = clear.input.len();

    assert_eq!(
        (new.submit_input(&catalog), clear.submit_input(&catalog)),
        (UiAction::New, UiAction::Clear)
    );
}

#[test]
fn composer_queues_a_new_turn_without_steering_middleware() {
    let catalog = UiCatalog::build(
        &[],
        &[],
        std::path::Path::new("/missing-horus-test-workspace"),
    )
    .expect("UI catalog");
    let mut working = state();
    working.active_turn = Some("turn".into());
    working.input = "next task".into();
    working.cursor = working.input.len();

    assert_eq!(
        working.submit_input(&catalog),
        UiAction::Submit(Op::UserInput {
            text: "next task".into(),
            attachments: Vec::new(),
        })
    );
}

#[test]
fn composer_targets_interrupt_at_the_active_turn() {
    let catalog = default_catalog();
    let mut slash = state();
    slash.active_turn = Some("turn-1".into());
    slash.input = "/interrupt".into();
    slash.cursor = slash.input.len();
    let slash_action = slash.submit_input(&catalog);

    let mut escape = state();
    escape.active_turn = Some("turn-1".into());
    let escape_action =
        escape.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &catalog);

    assert_eq!(
        (slash_action, escape_action),
        (
            UiAction::Submit(Op::Interrupt {
                turn_id: "turn-1".into()
            }),
            UiAction::Submit(Op::Interrupt {
                turn_id: "turn-1".into()
            })
        )
    );
}

#[test]
fn generic_picker_submits_the_selected_operation() {
    let mut state = state();
    state.handle_agent_event(
        EventMsg::Frontend(FrontendEvent::Picker {
            title: "Resume chat".into(),
            options: vec![
                horus::protocol::FrontendPickerOption {
                    label: "first".into(),
                    description: "older".into(),
                    detail: String::new(),
                    symbol: None,
                    shows_detail: false,
                    op: Op::ResumeSession {
                        session_id: "first".into(),
                    },
                },
                horus::protocol::FrontendPickerOption {
                    label: "second".into(),
                    description: "newer".into(),
                    detail: String::new(),
                    symbol: None,
                    shows_detail: false,
                    op: Op::ResumeSession {
                        session_id: "second".into(),
                    },
                },
            ],
        }),
        Vec::new(),
    );

    let catalog = default_catalog();
    state.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &catalog);

    assert_eq!(
        state.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &catalog),
        UiAction::Submit(Op::ResumeSession {
            session_id: "second".into(),
        })
    );
}

#[tokio::test]
async fn workspace_reference_menu_inserts_a_file() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::create_dir(workspace.path().join("src")).expect("source directory");
    std::fs::write(workspace.path().join("src/lib.rs"), "").expect("source file");
    let catalog = catalog(workspace.path());
    catalog
        .start_workspace_inventory(true)
        .await
        .expect("workspace inventory");
    let mut state = state();
    state.input = "review @lib".into();
    state.cursor = state.input.len();

    state.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &catalog);

    assert_eq!(state.input, "review src/lib.rs");
}

#[tokio::test]
async fn remote_workspace_inventory_does_not_read_the_client_filesystem() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(workspace.path().join("client-only.rs"), "").expect("client file");
    let catalog = catalog(workspace.path());
    catalog
        .start_workspace_inventory(false)
        .await
        .expect("disabled workspace inventory");

    assert!(catalog.reference_suggestions('@', "client").is_empty());
}

#[test]
fn composer_rejects_input_over_the_protocol_limit() {
    let mut state = state();
    state.insert_paste(&"x".repeat(horus::protocol::MAX_USER_INPUT_BYTES));
    state.insert_text("x");

    assert_eq!(
        (
            state.pastes.values().map(String::len).sum::<usize>(),
            state.input_limit_reached,
        ),
        (horus::protocol::MAX_USER_INPUT_BYTES, true)
    );
}

#[test]
fn option_backspace_deletes_the_previous_word_and_trailing_space() {
    let mut state = state();
    state.input = "hello   world  ".into();
    state.cursor = state.input.len();

    state.handle_key(
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT),
        &default_catalog(),
    );

    assert_eq!((state.input.as_str(), state.cursor), ("hello   ", 8));
}

#[test]
fn option_backspace_deletes_a_collapsed_paste_atomically() {
    let mut state = state();
    state.input = "foo.".into();
    state.cursor = state.input.len();
    state.insert_paste("pasted\ncontent");

    state.handle_key(
        KeyEvent::new(KeyCode::Backspace, KeyModifiers::ALT),
        &default_catalog(),
    );

    assert_eq!((state.input.as_str(), state.pastes.len()), ("foo.", 0));
}

#[test]
fn arrow_up_recalls_composer_history_and_ctrl_t_toggles_transcript() {
    let catalog = default_catalog();
    let mut state = state();
    state.remember_composer_input("previous prompt".into());

    state.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), &catalog);
    assert_eq!(state.input, "previous prompt");
    assert!(state.preview.is_none());

    state.handle_key(
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        &catalog,
    );
    assert!(matches!(
        state.preview.as_ref().map(|preview| &preview.content),
        Some(PreviewContent::LiveTranscript)
    ));

    state.handle_key(
        KeyEvent::new(KeyCode::Char('t'), KeyModifiers::CONTROL),
        &catalog,
    );
    assert!(state.preview.is_none());
}

#[test]
fn approval_preserves_an_in_progress_draft() {
    let mut state = state();
    state.input = "steer after approval".into();
    state.cursor = state.input.len();

    state.handle_agent_event(
        EventMsg::ExecApprovalRequest(horus::protocol::ExecApprovalRequestEvent {
            id: "approval".into(),
            turn_id: "turn".into(),
            calls: Vec::new(),
            reason: "test".into(),
        }),
        Vec::new(),
    );

    assert_eq!(
        state.handle_key(
            KeyEvent::new_with_kind(
                KeyCode::Char('y'),
                KeyModifiers::NONE,
                ratatui::crossterm::event::KeyEventKind::Repeat,
            ),
            &default_catalog(),
        ),
        UiAction::None
    );
    assert_eq!(
        state.handle_key(
            KeyEvent::new(KeyCode::Char('Y'), KeyModifiers::SHIFT),
            &default_catalog(),
        ),
        UiAction::Submit(Op::ExecApproval {
            id: "approval".into(),
            decision: ReviewDecision::Approved,
        })
    );
    assert_eq!(state.input, "steer after approval");
    assert!(state.is_working());
}
