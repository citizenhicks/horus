//! Live gateway dashboard and gateway-scoped setup entrypoints.

mod runtime;
mod state;
mod view;

use std::env;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mobius::protocol::{
    EventMsg, FrontendActionListItem, FrontendBlock, FrontendEvent, FrontendListItemState,
    FrontendSlot, FrontendTone, FrontendWidget, FrontendWidgetContent, MAX_USER_INPUT_BYTES, Op,
    Submission,
};
use mobius::{Error, Result};
use mobius_gateway::client::{Endpoint, GatewayClient, GatewayEvents, GatewaySender};
use mobius_gateway::config::{ConfigStore, GatewayConfig};
use mobius_gateway::wire::{
    ClientKind, ClientMessage, ClientStatus, DailyUsage, ProfileSnapshot, ReadyPayload,
    ServerMessage, SessionActivityState, SessionReadyPayload, SessionRecord,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, HighlightSpacing, List, ListState, Paragraph, Wrap};
use tokio::time::MissedTickBehavior;
use uuid::Uuid;

use super::setup::{self, SetupMode};
use super::terminal::{INPUT_POLL, MAX_INPUT_BATCH, TerminalGuard, poll_event};
use super::theme::{Role, current};
use super::{provider_instance_label, terminal_text};
use crate::gateway_accounts::configured_token;

use self::runtime::*;
use self::state::*;
use self::view::*;

const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

pub async fn run(state_dir: PathBuf) -> Result<()> {
    let (sender, mut events, mut state) = connect(state_dir).await?;
    let _guard = TerminalGuard::alternate()?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    terminal.clear()?;
    dashboard_loop(&mut terminal, sender, &mut events, &mut state).await
}

pub async fn run_provider(state_dir: PathBuf) -> Result<()> {
    let (sender, mut events, mut state) = connect(state_dir).await?;
    let mut guard = TerminalGuard::alternate()?;
    guard.set_mouse_capture(false)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    setup::run_gateway(
        &mut terminal,
        SetupMode::Login,
        &sender,
        &mut events,
        &mut state.gateway,
    )
    .await
}

#[cfg(test)]
mod tests {
    use mobius::backend::checkpoint::SessionSummary;
    use mobius::protocol::{
        FrontendAction, FrontendActionListItem, FrontendBlock, FrontendBlockFormat,
        FrontendBlockRole, FrontendBlockState, FrontendBlockUpdate, FrontendEvent,
        FrontendListItemState, FrontendPickerOption, FrontendSlot, FrontendSymbol, FrontendTone,
        FrontendWidget, FrontendWidgetContent, Op, SessionContext, TokenUsage,
    };
    use mobius_gateway::wire::{DailyUsage, SessionActivity, SessionActivityState, SessionRecord};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::widgets::ListState;

    use super::{
        CapabilityOverlay, activate_overlay, handle_action_input_key, moved_index,
        ordered_sessions, prepare_overlay_operation, render_action_list, token_total_for_day,
    };

    #[test]
    fn daily_usage_sums_all_providers_on_the_same_day() {
        let usage = [
            DailyUsage {
                unix_day: 7,
                provider: "openai_socket".into(),
                usage: TokenUsage {
                    total_tokens: 11,
                    ..TokenUsage::default()
                },
            },
            DailyUsage {
                unix_day: 7,
                provider: "kimi".into(),
                usage: TokenUsage {
                    total_tokens: 13,
                    ..TokenUsage::default()
                },
            },
            DailyUsage {
                unix_day: 8,
                provider: "responses".into(),
                usage: TokenUsage {
                    total_tokens: 17,
                    ..TokenUsage::default()
                },
            },
        ];

        assert_eq!(token_total_for_day(&usage, 7), 24);
    }

    #[test]
    fn moved_index_clamps_scroll_to_the_history() {
        assert_eq!(
            (
                moved_index(Some(1), 3, -5),
                moved_index(Some(1), 3, 5),
                moved_index(None, 0, 1),
            ),
            (Some(0), Some(2), None)
        );
    }

    #[test]
    fn session_identity_survives_activity_sorting() {
        let mut sessions = vec![
            session("selected", SessionActivityState::Idle),
            session("other", SessionActivityState::Running),
        ];
        assert_eq!(
            ordered_sessions(&sessions)
                .iter()
                .position(|session| session.summary.session_id == "selected"),
            Some(1)
        );

        sessions[0].activity.state = SessionActivityState::Running;
        sessions[1].activity.state = SessionActivityState::Idle;
        assert_eq!(
            ordered_sessions(&sessions)
                .iter()
                .position(|session| session.summary.session_id == "selected"),
            Some(0)
        );
    }

    #[test]
    fn open_widget_tracks_updates_and_submits_the_advertised_operation() {
        let key: (String, String) = ("capability-a".into(), "view".into());
        let mut overlay = CapabilityOverlay {
            session_id: "session-1".into(),
            widgets: vec![(key.clone(), widget(blocks("Initial")))],
            widget_list: ListState::default(),
            open: Some(key.clone()),
            option_list: ListState::default(),
            action_index: 0,
            input: None,
        };
        overlay.sync_selection();
        let op = Op::SetModel {
            route: "route-a".into(),
        };
        overlay.apply(FrontendEvent::Widget {
            capability: key.0.clone(),
            item: widget(FrontendWidgetContent::Picker {
                title: "Updated".into(),
                options: vec![FrontendPickerOption {
                    label: "Apply".into(),
                    description: "Apply the advertised operation".into(),
                    detail: String::new(),
                    symbol: None,
                    shows_detail: false,
                    op: op.clone(),
                }],
            }),
        });

        assert!(matches!(
            overlay.open_widget().and_then(|widget| widget.content.as_ref()),
            Some(FrontendWidgetContent::Picker { title, .. }) if title == "Updated"
        ));
        assert_eq!(activate_overlay(&mut overlay), Some(op));

        overlay.apply(FrontendEvent::RemoveWidget {
            capability: key.0,
            id: key.1,
        });
        assert!(overlay.open.is_none());
    }

    #[test]
    fn opening_widget_submits_its_advertised_refresh() {
        let key: (String, String) = ("capability-a".into(), "view".into());
        let op = Op::CapabilityCommand {
            capability: key.0.clone(),
            command: "refresh".into(),
            arguments: String::new(),
            input: None,
            target: None,
        };
        let mut item = widget(blocks("Initial"));
        item.action = Some(op.clone());
        let mut overlay = CapabilityOverlay {
            session_id: "session-1".into(),
            widgets: vec![(key.clone(), item)],
            widget_list: ListState::default().with_selected(Some(0)),
            open: None,
            option_list: ListState::default(),
            action_index: 0,
            input: None,
        };

        assert_eq!(activate_overlay(&mut overlay), Some(op));
        assert_eq!(overlay.open, Some(key));
    }

    #[test]
    fn action_list_renders_one_row_with_declared_actions_and_runs_the_selected_one() {
        let edit = capability_op("edit", Some("Remember this"));
        let delete = capability_op("delete", None);
        let content = action_list(edit.clone(), delete.clone());
        let key: (String, String) = ("capability-a".into(), "view".into());
        let mut overlay = CapabilityOverlay {
            session_id: "session-1".into(),
            widgets: vec![(key.clone(), widget(content.clone()))],
            widget_list: ListState::default(),
            open: Some(key),
            option_list: ListState::default().with_selected(Some(0)),
            action_index: 1,
            input: None,
        };
        let FrontendWidgetContent::ActionList { title, items } = content else {
            unreachable!();
        };
        let mut terminal = Terminal::new(TestBackend::new(72, 5)).expect("terminal");

        terminal
            .draw(|frame| {
                render_action_list(
                    frame,
                    frame.area(),
                    &title,
                    &items,
                    &mut overlay.option_list,
                    overlay.action_index,
                );
            })
            .expect("action list draw");

        let rendered = terminal.backend().to_string();
        let row = rendered
            .lines()
            .find(|line| line.contains("Remember this"))
            .expect("note row");
        let note_position = row.find("Remember this").expect("note text");
        let edit_position = row.find("[Edit]").expect("edit action");
        let delete_position = row.find("[Delete]").expect("delete action");
        assert!(note_position < edit_position && edit_position < delete_position);
        assert!(row.len().saturating_sub(delete_position + "[Delete]".len()) <= 1);
        assert_eq!(activate_overlay(&mut overlay), Some(delete));
    }

    #[test]
    fn editable_action_replaces_its_advertised_input_before_submission() {
        let key: (String, String) = ("capability-a".into(), "view".into());
        let mut overlay = CapabilityOverlay {
            session_id: "session-1".into(),
            widgets: vec![(key.clone(), widget(blocks("Initial")))],
            widget_list: ListState::default(),
            open: Some(key),
            option_list: ListState::default(),
            action_index: 0,
            input: None,
        };

        assert!(
            prepare_overlay_operation(&mut overlay, capability_op("edit", Some("Before")))
                .is_none()
        );
        handle_action_input_key(
            &mut overlay,
            KeyEvent::new(KeyCode::Char('!'), KeyModifiers::NONE),
        );
        let submitted = handle_action_input_key(
            &mut overlay,
            KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        )
        .expect("submitted operation");

        assert!(matches!(
            submitted,
            Op::CapabilityCommand { input: Some(value), .. } if value == "Before!"
        ));
    }

    fn session(id: &str, state: SessionActivityState) -> SessionRecord {
        SessionRecord {
            summary: SessionSummary {
                session_id: id.into(),
                session_context: SessionContext::default(),
                parent_session_id: None,
                parent_sequence: None,
                sequence: 0,
                catalog_visible: true,
                first_user_message: None,
                execution_stats: Default::default(),
                created_at: 0,
                updated_at: 0,
            },
            title: None,
            pinned: false,
            activity: SessionActivity {
                state,
                ..SessionActivity::default()
            },
        }
    }

    fn blocks(text: &str) -> FrontendWidgetContent {
        FrontendWidgetContent::Blocks {
            title: "View".into(),
            blocks: vec![FrontendBlock {
                id: None,
                group: None,
                update: FrontendBlockUpdate::Replace,
                state: FrontendBlockState::Complete,
                role: FrontendBlockRole::Notice,
                title: String::new(),
                text: text.into(),
                symbol: None,
                format: FrontendBlockFormat::PlainText,
                tone: FrontendTone::Neutral,
                files: Vec::new(),
            }],
        }
    }

    fn action_list(edit: Op, delete: Op) -> FrontendWidgetContent {
        FrontendWidgetContent::ActionList {
            title: "Notes".into(),
            items: vec![FrontendActionListItem {
                id: "note-1".into(),
                text: "Remember this".into(),
                state: FrontendListItemState::Plain,
                actions: vec![
                    FrontendAction {
                        id: "edit".into(),
                        label: "Edit".into(),
                        symbol: FrontendSymbol::Edit,
                        tone: FrontendTone::Neutral,
                        op: edit,
                    },
                    FrontendAction {
                        id: "delete".into(),
                        label: "Delete".into(),
                        symbol: FrontendSymbol::Delete,
                        tone: FrontendTone::Error,
                        op: delete,
                    },
                ],
            }],
        }
    }

    fn capability_op(command: &str, input: Option<&str>) -> Op {
        Op::CapabilityCommand {
            capability: "capability-a".into(),
            command: command.into(),
            arguments: "note-1".into(),
            input: input.map(str::to_owned),
            target: None,
        }
    }

    fn widget(content: FrontendWidgetContent) -> FrontendWidget {
        FrontendWidget {
            id: "view".into(),
            slot: FrontendSlot::Navigation,
            text: "Capability view".into(),
            tone: FrontendTone::Neutral,
            symbol: None,
            icon_only: false,
            progress: None,
            content: Some(content),
            action: None,
        }
    }
}
