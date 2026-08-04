use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::crossterm::event::{MouseEvent, MouseEventKind};
use ratatui::style::Color;

use super::*;
use crate::frontend::catalog::UiCatalog;
use crate::frontend::theme::{Role, current};
use horus::backend::model::ModelChoice;
use horus::protocol::{
    FrontendActiveInput, FrontendBlockFormat, FrontendContribution, FrontendSlot, FrontendTone,
    FrontendWidget, ReviewDecision,
};
use horus_gateway::wire::{RenderedEvent, RenderedPreview};

fn catalog(workspace: &std::path::Path) -> UiCatalog {
    let steering = FrontendContribution {
        capability: "steering".into(),
        active_input: Some(FrontendActiveInput {
            operation: "steer".into(),
        }),
        ..FrontendContribution::default()
    };
    UiCatalog::build(
        &[steering],
        &[ModelChoice {
            route: "kimi".into(),
            group: "kimi".into(),
            model: "kimi-k3".into(),
            reasoning_effort: Some("high".into()),
            context_window: Some(1_048_576),
        }],
        workspace,
    )
    .expect("UI catalog")
}

fn default_catalog() -> UiCatalog {
    catalog(std::path::Path::new("/missing-horus-test-workspace"))
}

fn state() -> TuiState {
    TuiState::new(
        &default_catalog(),
        "/work/horus".into(),
        ModelInfo {
            model: "kimi-k3".into(),
            reasoning_effort: Some("high".into()),
        },
        "kimi".into(),
    )
}

fn rendered_text(lines: &[ratatui::text::Line<'_>]) -> String {
    lines
        .iter()
        .map(|line| {
            line.spans
                .iter()
                .map(|span| span.content.as_ref())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

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
                    op: Op::ResumeSession {
                        session_id: "first".into(),
                    },
                },
                horus::protocol::FrontendPickerOption {
                    label: "second".into(),
                    description: "newer".into(),
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

#[test]
fn completed_diff_replaces_the_pending_block_with_a_styled_diff() {
    let mut state = state();
    state.transcript.clear();
    state.apply_block(FrontendBlock {
        id: Some("turn/patch".into()),
        group: None,
        append: false,
        pending: true,
        text: "◉ Edit note.rs".into(),
        format: FrontendBlockFormat::PlainText,
        tone: FrontendTone::Neutral,
    });
    view::live_transcript_lines(&mut state, 0, 80);
    assert_eq!(
        state
            .transcript
            .front()
            .and_then(|entry| entry.rendered.as_ref())
            .map(|(width, _)| *width),
        Some(80)
    );
    state.apply_block(FrontendBlock {
        id: Some("turn/patch".into()),
        group: None,
        append: false,
        pending: false,
        text: "--- note.rs\n+++ note.rs\n@@ -1,5 +1,5 @@\n-fn old_name() {}\n+fn new_name() {}\n keep_one();\n-let removed = false;\n keep_two();\n+let added = true;\n keep_three();\n".into(),
        format: FrontendBlockFormat::UnifiedDiff,
        tone: FrontendTone::Success,
    });
    assert!(
        state
            .transcript
            .front()
            .is_some_and(|entry| entry.rendered.is_none())
    );

    assert_eq!(
        state.transcript.front().map(|entry| entry.format),
        Some(FrontendBlockFormat::UnifiedDiff)
    );
    let lines = view::live_transcript_lines(&mut state, 0, 80);
    let text = rendered_text(&lines);
    assert!(text.contains("◉ Edited note.rs (+2 -2)"), "{text}");
    assert!(text.contains("    1 -fn old_name() {}"), "{text}");
    assert!(text.contains("    1 +fn new_name() {}"), "{text}");
    assert!(!text.contains("◉ Edit note.rs"), "{text}");

    let changed_delete = lines
        .iter()
        .find(|line| rendered_text(std::slice::from_ref(line)).contains("-fn old_name"))
        .expect("changed delete");
    let changed_insert = lines
        .iter()
        .find(|line| rendered_text(std::slice::from_ref(line)).contains("+fn new_name"))
        .expect("changed insert");
    let pure_delete = lines
        .iter()
        .find(|line| rendered_text(std::slice::from_ref(line)).contains("-let removed"))
        .expect("pure delete");
    let pure_insert = lines
        .iter()
        .find(|line| rendered_text(std::slice::from_ref(line)).contains("+let added"))
        .expect("pure insert");

    assert_eq!(
        changed_delete.style.bg,
        Some(current().diff_delete_background())
    );
    assert_eq!(
        changed_insert.style.bg,
        Some(current().diff_add_background())
    );
    assert_eq!(
        pure_delete.style.bg,
        Some(current().diff_delete_background())
    );
    assert_eq!(pure_insert.style.bg, Some(current().diff_add_background()));
    assert!(
        [&changed_delete, &changed_insert, &pure_delete, &pure_insert]
            .into_iter()
            .all(|line| line.width() == 80)
    );
    assert!(lines.iter().flat_map(|line| &line.spans).any(|span| {
        span.content == "fn" && span.style.fg != Some(current().color(Role::Text))
    }));

    let narrow_lines = view::live_transcript_lines(&mut state, 0, 40);
    let narrow_insert = narrow_lines
        .iter()
        .find(|line| rendered_text(std::slice::from_ref(line)).contains("+fn new_name"))
        .expect("narrow insert");
    assert_eq!(
        (
            state
                .transcript
                .front()
                .and_then(|entry| entry.rendered.as_ref())
                .map(|(width, _)| *width),
            narrow_insert.width(),
        ),
        (Some(40), 40)
    );
}

#[test]
fn transcript_keeps_a_bounded_recent_window() {
    let mut state = state();
    state.transcript.clear();
    for index in 0..=MAX_TRANSCRIPT_ENTRIES {
        state.push_entry(format!("message {index}"), TranscriptTone::Neutral);
    }

    assert_eq!(state.transcript.len(), MAX_TRANSCRIPT_ENTRIES);
    assert_eq!(
        state.transcript.front().map(|entry| entry.text.as_str()),
        Some("message 1")
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
fn capability_header_is_live_styled_and_transparent() {
    let catalog = default_catalog();
    let mut state = state();
    state.transcript.clear();
    state.widgets.insert(
        ("skills".into(), "count".into()),
        FrontendWidget {
            id: "count".into(),
            slot: FrontendSlot::Header,
            text: "skills 2".into(),
            tone: FrontendTone::Neutral,
            action: None,
        },
    );
    let mut terminal = Terminal::new(TestBackend::new(50, 15)).expect("terminal");
    terminal
        .draw(|frame| view::render(frame, &mut state, &catalog))
        .expect("live pane draw");
    let live_pane = terminal.backend().to_string();
    let skill_cell = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .find(|cell| cell.symbol() == "2")
        .expect("styled capability cell");

    assert!(live_pane.contains("skills 2"));
    assert_eq!(
        (skill_cell.fg, skill_cell.bg),
        (current().color(Role::Neutral), Color::Reset)
    );
}

#[test]
fn sora_transcript_stays_styled_and_transparent_in_chat_and_preview() {
    let catalog = default_catalog();
    let mut state = state();
    state.transcript.clear();
    state.push_entry("λ".into(), TranscriptTone::Warning);
    let mut terminal = Terminal::new(TestBackend::new(40, 16)).expect("terminal");

    terminal
        .draw(|frame| view::render(frame, &mut state, &catalog))
        .expect("chat draw");
    let chat_cell = terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .find(|cell| cell.symbol() == "λ")
        .expect("styled chat cell");
    assert_eq!(
        (chat_cell.fg, chat_cell.bg),
        (current().color(Role::Warning), Color::Reset)
    );

    state.open_transcript_preview();
    terminal
        .draw(|frame| view::render_preview(frame, &mut state))
        .expect("preview draw");
    let preview = terminal.backend().buffer();
    let preview_cell = preview
        .content()
        .iter()
        .find(|cell| cell.symbol() == "λ")
        .expect("styled preview cell");

    assert_eq!(
        (preview_cell.fg, preview_cell.bg),
        (current().color(Role::Warning), Color::Reset)
    );
    assert!(preview.content().iter().all(|cell| cell.bg == Color::Reset));
}

#[test]
fn live_transcript_preview_uses_the_full_frame_and_new_entries() {
    let mut state = state();
    state.transcript.clear();
    state.open_transcript_preview();
    state.push_entry("new live row".into(), TranscriptTone::Neutral);
    let mut terminal = Terminal::new(TestBackend::new(40, 16)).expect("terminal");

    terminal
        .draw(|frame| view::render_preview(frame, &mut state))
        .expect("preview draw");
    let rendered = terminal.backend().to_string();

    assert!(rendered.contains("new live row"));
    assert!(
        rendered
            .lines()
            .next()
            .is_some_and(|line| line.contains('┌')),
        "{rendered}"
    );
    assert!(
        rendered
            .lines()
            .last()
            .is_some_and(|line| line.contains('└')),
        "{rendered}"
    );
}

#[test]
fn snapshot_preview_scrolls_with_the_mouse_wheel() {
    let mut state = state();
    events::handle_gateway_event(
        &mut state,
        EventMsg::ContextCompacted,
        Vec::new(),
        None,
        Some(RenderedPreview {
            title: "subagent".into(),
            events: (0..30)
                .map(|index| RenderedEvent {
                    event: EventMsg::UserMessage(horus::protocol::UserMessageEvent {
                        message: format!("subagent row {index}"),
                    }),
                    blocks: Vec::new(),
                })
                .collect(),
        }),
    );
    let mut terminal = Terminal::new(TestBackend::new(40, 10)).expect("terminal");
    terminal
        .draw(|frame| view::render_preview(frame, &mut state))
        .expect("preview draw");
    let bottom = terminal.backend().to_string();
    assert!(bottom.contains("subagent row 29"), "{bottom}");

    assert!(!state.handle_mouse(MouseEvent {
        kind: MouseEventKind::Moved,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    }));
    assert!(state.handle_mouse(MouseEvent {
        kind: MouseEventKind::ScrollUp,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    }));
    terminal
        .draw(|frame| view::render_preview(frame, &mut state))
        .expect("scrolled preview draw");
    let scrolled = terminal.backend().to_string();
    assert!(scrolled.contains("subagent row 24"), "{scrolled}");
    assert!(!scrolled.contains("subagent row 29"), "{scrolled}");

    state.handle_mouse(MouseEvent {
        kind: MouseEventKind::ScrollDown,
        column: 0,
        row: 0,
        modifiers: KeyModifiers::NONE,
    });
    terminal
        .draw(|frame| view::render_preview(frame, &mut state))
        .expect("restored preview draw");
    let restored = terminal.backend().to_string();
    assert!(restored.contains("subagent row 29"), "{restored}");
}

#[test]
fn chat_transcript_scrolls_with_the_mouse_wheel() {
    let catalog = default_catalog();
    let mut state = state();
    state.transcript.clear();
    for index in 0..30 {
        state.push_entry(format!("chat row {index}"), TranscriptTone::Neutral);
    }
    let mut terminal = Terminal::new(TestBackend::new(40, 10)).expect("terminal");
    terminal
        .draw(|frame| view::render(frame, &mut state, &catalog))
        .expect("chat draw");
    assert!(
        terminal.backend().to_string().contains("chat row 29"),
        "{}",
        terminal.backend()
    );

    for _ in 0..2 {
        state.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollUp,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
    }
    terminal
        .draw(|frame| view::render(frame, &mut state, &catalog))
        .expect("scrolled chat draw");
    let scrolled = terminal.backend().to_string();
    assert!(!scrolled.contains("chat row 29"), "{scrolled}");

    for _ in 0..2 {
        state.handle_mouse(MouseEvent {
            kind: MouseEventKind::ScrollDown,
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        });
    }
    terminal
        .draw(|frame| view::render(frame, &mut state, &catalog))
        .expect("restored chat draw");
    assert!(
        terminal.backend().to_string().contains("chat row 29"),
        "{}",
        terminal.backend()
    );
}

#[test]
fn bordered_composer_grows_to_show_wrapped_input() {
    let catalog = default_catalog();
    let mut state = state();
    state.transcript.clear();
    state.input = "first line\nsecond line\nthird line\nfourth line\nfifth line".into();
    state.cursor = state.input.len();
    let mut terminal = Terminal::new(TestBackend::new(30, 16)).expect("terminal");

    terminal
        .draw(|frame| view::render(frame, &mut state, &catalog))
        .expect("draw");
    let rendered = terminal.backend().to_string();

    assert!(rendered.contains("first line"));
    assert!(rendered.contains("fifth line"));
}

#[test]
fn capped_composer_keeps_a_wide_wrapped_cursor_visible() {
    let catalog = default_catalog();
    let mut state = state();
    state.transcript.clear();
    state.input = "界".repeat(200);
    state.cursor = state.input.len();
    let mut terminal = Terminal::new(TestBackend::new(20, 10)).expect("terminal");

    terminal
        .draw(|frame| view::render(frame, &mut state, &catalog))
        .expect("draw");

    assert!(terminal.backend().to_string().contains('█'));
}

#[test]
fn session_card_keeps_the_eye_and_session_details() {
    let mut state = state();
    state.cwd = "/work/horus.nosync".into();
    let card = view::welcome_card(&state);

    assert!(card.contains("⣠⡤⢶"));
    assert!(card.contains("HORUS v"));
    assert!(card.contains("model: kimi-k3 high"));
    assert!(card.contains("horus.nosync"));
}

#[test]
fn narrow_terminal_keeps_session_card_and_compact_footer() {
    let catalog = default_catalog();
    let mut state = state();
    state.cwd = "/work/horus".into();
    let mut terminal = Terminal::new(TestBackend::new(50, 15)).expect("terminal");

    terminal
        .draw(|frame| view::render(frame, &mut state, &catalog))
        .expect("draw");
    let rendered = terminal.backend().to_string();

    assert!(rendered.contains("⣠⡤⢶"), "{rendered}");
    assert!(rendered.contains("directory: /work/horus"), "{rendered}");
    assert!(rendered.contains("kimi-k3 high · horus"), "{rendered}");
    assert!(rendered.contains("╭"), "{rendered}");
}

#[test]
fn commentary_is_transient_until_final_output_starts() {
    let mut state = state();
    state.active_turn = Some("turn".into());
    state.handle_agent_event(
        EventMsg::AgentMessageContentDelta(horus::protocol::AgentMessageContentDeltaEvent {
            thread_id: "thread".into(),
            turn_id: "turn".into(),
            item_id: "commentary".into(),
            delta: "Checking the workspace".into(),
            phase: Some(AgentMessagePhase::Commentary),
        }),
        Vec::new(),
    );

    assert_eq!(state.status_message, "Checking the workspace");
    assert!(state.streaming.is_empty());

    state.handle_agent_event(
        EventMsg::AgentMessageContentDelta(horus::protocol::AgentMessageContentDeltaEvent {
            thread_id: "thread".into(),
            turn_id: "turn".into(),
            item_id: "answer".into(),
            delta: "Done".into(),
            phase: Some(AgentMessagePhase::FinalAnswer),
        }),
        Vec::new(),
    );

    assert!(state.status_message.is_empty());
    assert_eq!(state.streaming, "Done");
}

#[test]
fn final_message_replaces_an_incomplete_stream() {
    let mut state = state();
    state.streaming = "partial".into();

    state.handle_agent_event(
        EventMsg::AgentMessage(horus::protocol::AgentMessageEvent {
            message: "complete answer".into(),
            phase: Some(AgentMessagePhase::FinalAnswer),
        }),
        Vec::new(),
    );

    assert!(state.streaming.is_empty());
    assert_eq!(
        state.transcript.back().map(|entry| entry.text.as_str()),
        Some("complete answer")
    );
}

#[test]
fn gateway_history_preserves_child_diff_rendering() {
    let mut state = state();
    let message = EventMsg::AgentMessage(horus::protocol::AgentMessageEvent {
        message: "changed the file".into(),
        phase: Some(AgentMessagePhase::FinalAnswer),
    });
    let history_event = EventMsg::SessionHistory(horus::protocol::SessionHistoryEvent {
        events: vec![message.clone()],
    });

    events::handle_gateway_event(
        &mut state,
        history_event,
        Vec::new(),
        Some(vec![RenderedEvent {
            event: message,
            blocks: vec![FrontendBlock {
                id: None,
                group: None,
                append: false,
                pending: false,
                text: "--- a/file\n+++ b/file\n-old\n+new".into(),
                format: FrontendBlockFormat::UnifiedDiff,
                tone: FrontendTone::Neutral,
            }],
        }]),
        None,
    );

    let entry = state.transcript.back().expect("rendered history entry");
    assert_eq!(entry.format, FrontendBlockFormat::UnifiedDiff);
    assert_eq!(entry.text, "--- a/file\n+++ b/file\n-old\n+new");
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

#[test]
fn transcript_text_strips_terminal_control_characters() {
    let mut state = state();
    state.push("unsafe \u{1b}[31mred\u{1b}[0m", TranscriptTone::Warning);

    assert_eq!(
        state.transcript.back().expect("entry").text,
        "unsafe [31mred[0m"
    );
}
