//! Live gateway dashboard and gateway-scoped setup entrypoints.

use std::env;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use horus::protocol::{
    EventMsg, FrontendActionListItem, FrontendBlock, FrontendEvent, FrontendListItemState,
    FrontendSlot, FrontendTone, FrontendWidget, FrontendWidgetContent, MAX_USER_INPUT_BYTES, Op,
    Submission,
};
use horus::{Error, Result};
use horus_gateway::client::{Endpoint, GatewayClient, GatewayEvents, GatewaySender};
use horus_gateway::config::{ConfigStore, GatewayConfig};
use horus_gateway::wire::{
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
use super::terminal_text;
use super::theme::{Role, current};
use crate::gateway_accounts::configured_token;

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

struct DashboardState {
    endpoint: String,
    gateway: ReadyPayload,
    clients: Vec<ClientStatus>,
    current_client_id: Option<String>,
    selected_client_id: Option<String>,
    selected_session_id: Option<String>,
    device_list: ListState,
    chat_list: ListState,
    focus: DashboardFocus,
    pending_unpair: Option<(String, String)>,
    profile: Option<ProfileSnapshot>,
    pending_open: Option<(String, String)>,
    overlay: Option<CapabilityOverlay>,
    error: Option<String>,
}

struct CapabilityOverlay {
    session_id: String,
    widgets: Vec<((String, String), FrontendWidget)>,
    widget_list: ListState,
    open: Option<(String, String)>,
    option_list: ListState,
    action_index: usize,
    input: Option<ActionInput>,
}

struct ActionInput {
    op: Op,
    text: String,
    cursor: usize,
}

impl CapabilityOverlay {
    fn from_session(payload: SessionReadyPayload) -> Self {
        let session_id = payload.session.session_id;
        let widgets = payload
            .contributions
            .into_iter()
            .flat_map(|contribution| {
                contribution.widgets.into_iter().filter_map(move |item| {
                    (item.slot == FrontendSlot::Navigation)
                        .then(|| ((contribution.capability.clone(), item.id.clone()), item))
                })
            })
            .collect();
        let mut overlay = Self {
            session_id,
            widgets,
            widget_list: ListState::default(),
            open: None,
            option_list: ListState::default(),
            action_index: 0,
            input: None,
        };
        for widget in payload.widgets {
            overlay.apply(FrontendEvent::Widget {
                capability: widget.capability,
                item: widget.item,
            });
        }
        overlay.sync_selection();
        overlay
    }

    fn apply(&mut self, event: FrontendEvent) {
        match event {
            FrontendEvent::Widget { capability, item } => {
                let key = (capability, item.id.clone());
                if item.slot == FrontendSlot::Navigation {
                    if let Some((_, widget)) = self
                        .widgets
                        .iter_mut()
                        .find(|(candidate, _)| candidate == &key)
                    {
                        *widget = item;
                    } else {
                        self.widgets.push((key, item));
                    }
                } else {
                    self.widgets.retain(|(candidate, _)| candidate != &key);
                }
            }
            FrontendEvent::RemoveWidget { capability, id } => {
                let key = (capability, id);
                self.widgets.retain(|(candidate, _)| candidate != &key);
            }
            _ => return,
        }
        self.sync_selection();
    }

    fn sync_selection(&mut self) {
        let selected = self
            .widgets
            .len()
            .checked_sub(1)
            .map(|last| self.widget_list.selected().unwrap_or_default().min(last));
        self.widget_list.select(selected);
        if self
            .open
            .as_ref()
            .is_some_and(|key| self.widget(key).is_none())
        {
            self.open = None;
        }
        let options = self
            .open_widget()
            .and_then(|widget| match widget.content.as_ref() {
                Some(FrontendWidgetContent::Picker { options, .. }) => Some(options.len()),
                Some(FrontendWidgetContent::ActionList { items, .. }) => Some(items.len()),
                _ => None,
            })
            .unwrap_or_default();
        self.option_list.select(
            options
                .checked_sub(1)
                .map(|last| self.option_list.selected().unwrap_or_default().min(last)),
        );
        self.action_index = self
            .selected_action_list_item()
            .and_then(|item| item.actions.len().checked_sub(1))
            .map_or(0, |last| self.action_index.min(last));
    }

    fn selected_key(&self) -> Option<(String, String)> {
        self.widgets
            .get(self.widget_list.selected()?)
            .map(|(key, _)| key)
            .cloned()
    }

    fn open_widget(&self) -> Option<&FrontendWidget> {
        self.widget(self.open.as_ref()?)
    }

    fn widget(&self, key: &(String, String)) -> Option<&FrontendWidget> {
        self.widgets
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, widget)| widget)
    }

    fn selected_action_list_item(&self) -> Option<&FrontendActionListItem> {
        let FrontendWidgetContent::ActionList { items, .. } =
            self.open_widget()?.content.as_ref()?
        else {
            return None;
        };
        items.get(self.option_list.selected()?)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DashboardFocus {
    Devices,
    Chats,
}

struct DashboardAreas {
    header: Rect,
    devices: Rect,
    chats: Rect,
    providers: Rect,
    defaults: Rect,
    usage: Rect,
    footer: Rect,
}

async fn connect(state_dir: PathBuf) -> Result<(GatewaySender, GatewayEvents, DashboardState)> {
    let (_, config) = ConfigStore::open(state_dir.clone()).map_err(gateway_error)?;
    let endpoint = dashboard_endpoint(&config)?;
    horus_gateway::command::ensure_background_gateway(state_dir)
        .await
        .map_err(gateway_error)?;
    let token = configured_token(&endpoint)
        .map_err(gateway_error)?
        .ok_or_else(|| {
            Error::Config(format!(
                "this machine is not paired with {endpoint}; pair it before opening the gateway dashboard"
            ))
        })?;
    let client = GatewayClient::connect(&endpoint, token, ClientKind::GatewayDashboard)
        .await
        .map_err(gateway_error)?;
    let (sender, mut events) = client.into_parts();
    let gateway = wait_ready(&mut events).await?;
    Ok((
        sender,
        events,
        DashboardState {
            endpoint: endpoint.to_string(),
            gateway,
            clients: Vec::new(),
            current_client_id: None,
            selected_client_id: None,
            selected_session_id: None,
            device_list: ListState::default(),
            chat_list: ListState::default(),
            focus: DashboardFocus::Devices,
            pending_unpair: None,
            profile: None,
            pending_open: None,
            overlay: None,
            error: None,
        },
    ))
}

fn dashboard_endpoint(config: &GatewayConfig) -> Result<Endpoint> {
    if config.tls.is_some() {
        if env::var_os("HORUS_GATEWAY_ENDPOINT").is_none() {
            return Err(Error::Config(
                "TLS dashboards require HORUS_GATEWAY_ENDPOINT with the certificate hostname"
                    .into(),
            ));
        }
        return Endpoint::from_env().map_err(gateway_error);
    }
    format!("tcp://{}", config.listen)
        .parse()
        .map_err(gateway_error)
}

async fn wait_ready(events: &mut GatewayEvents) -> Result<ReadyPayload> {
    loop {
        let frame = events
            .next()
            .await
            .map_err(gateway_error)?
            .ok_or_else(|| Error::Stopped("gateway disconnected before it was ready".into()))?;
        match frame.message {
            ServerMessage::Ready { payload } => return Ok(payload),
            ServerMessage::Error { message, .. } => return Err(Error::Stopped(message)),
            _ => {}
        }
    }
}

async fn dashboard_loop(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    sender: GatewaySender,
    events: &mut GatewayEvents,
    state: &mut DashboardState,
) -> Result<()> {
    let mut input = tokio::time::interval(INPUT_POLL);
    input.set_missed_tick_behavior(MissedTickBehavior::Skip);
    let mut refresh = tokio::time::interval(REFRESH_INTERVAL);
    refresh.set_missed_tick_behavior(MissedTickBehavior::Skip);
    sync_chat_selection(state);
    let mut dirty = true;
    loop {
        if dirty {
            terminal.draw(|frame| render(frame, state))?;
            dirty = false;
        }
        tokio::select! {
            _ = refresh.tick() => {
                request_snapshot(&sender).await?;
            }
            _ = input.tick() => {
                for _ in 0..MAX_INPUT_BATCH {
                    let Some(event) = poll_event()? else { break; };
                    dirty = true;
                    match event {
                        Event::Key(key)
                            if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) =>
                        {
                            if handle_key(terminal, &sender, events, state, key).await? {
                                return Ok(());
                            }
                        }
                        Event::Paste(text) => handle_overlay_paste(state, &text),
                        Event::Mouse(mouse) => {
                            handle_mouse(state, mouse, terminal.size()?.into());
                        }
                        _ => {}
                    }
                }
            }
            frame = events.next() => {
                let frame = frame
                    .map_err(gateway_error)?
                    .ok_or_else(|| Error::Stopped("gateway disconnected".into()))?;
                handle_frame(state, frame.message)?;
                dirty = true;
            }
        }
    }
}

async fn handle_key(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    state: &mut DashboardState,
    key: KeyEvent,
) -> Result<bool> {
    if key.modifiers.contains(KeyModifiers::CONTROL) && matches!(key.code, KeyCode::Char('c' | 'd'))
    {
        return Ok(true);
    }
    if state.overlay.is_some() {
        handle_overlay_key(sender, state, key).await?;
        return Ok(false);
    }
    if state.pending_unpair.is_some() {
        match key.code {
            KeyCode::Char('y') => confirm_unpair(sender, state).await?,
            KeyCode::Char('n') | KeyCode::Esc => state.pending_unpair = None,
            _ => {}
        }
        return Ok(false);
    }
    if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {
        return Ok(true);
    }
    if handle_navigation_key(state, key, terminal.size()?.into()) {
        return Ok(false);
    }
    if state.focus == DashboardFocus::Devices
        && matches!(key.code, KeyCode::Char('u') | KeyCode::Delete)
    {
        begin_unpair(state);
        return Ok(false);
    }
    if state.focus == DashboardFocus::Chats && key.code == KeyCode::Enter {
        open_selected_session(sender, state).await?;
        return Ok(false);
    }
    let mode = match key.code {
        KeyCode::Char('p') => Some(SetupMode::Login),
        KeyCode::Char('d') => Some(SetupMode::Agent),
        KeyCode::Char('r') => {
            state.error = None;
            request_snapshot(sender).await?;
            None
        }
        _ => None,
    };
    if let Some(mode) = mode {
        state.error = setup::run_gateway(terminal, mode, sender, events, &mut state.gateway)
            .await
            .err()
            .map(|error| error.to_string());
        terminal.clear()?;
        request_snapshot(sender).await?;
    }
    Ok(false)
}

async fn open_selected_session(sender: &GatewaySender, state: &mut DashboardState) -> Result<()> {
    let Some(session_id) = state.selected_session_id.clone() else {
        return Ok(());
    };
    let request_id = Uuid::new_v4().to_string();
    sender
        .send(ClientMessage::OpenSession {
            request_id: request_id.clone(),
            session_id: session_id.clone(),
            last_sequence: None,
        })
        .await
        .map_err(gateway_error)?;
    state.pending_open = Some((request_id, session_id));
    state.error = None;
    Ok(())
}

async fn handle_overlay_key(
    sender: &GatewaySender,
    state: &mut DashboardState,
    key: KeyEvent,
) -> Result<()> {
    let Some(overlay) = state.overlay.as_mut() else {
        return Ok(());
    };
    if overlay.input.is_some() {
        if let Some(op) = handle_action_input_key(overlay, key) {
            submit_operation(sender, &overlay.session_id, op).await?;
        }
        return Ok(());
    }
    let action_list_open = matches!(
        overlay
            .open_widget()
            .and_then(|widget| widget.content.as_ref()),
        Some(FrontendWidgetContent::ActionList { .. })
    );
    match key.code {
        KeyCode::Esc | KeyCode::Char('q') => {
            if overlay.open.take().is_none() {
                state.overlay = None;
            }
        }
        KeyCode::Left if action_list_open => move_overlay_action(overlay, -1),
        KeyCode::Left => {
            overlay.open = None;
        }
        KeyCode::Up | KeyCode::Char('k') => move_overlay_selection(overlay, -1),
        KeyCode::Down | KeyCode::Char('j') => move_overlay_selection(overlay, 1),
        KeyCode::Home => select_overlay_edge(overlay, false),
        KeyCode::End => select_overlay_edge(overlay, true),
        KeyCode::Right if action_list_open => move_overlay_action(overlay, 1),
        KeyCode::Enter | KeyCode::Right => {
            if let Some(op) = activate_overlay(overlay)
                && let Some(op) = prepare_overlay_operation(overlay, op)
            {
                submit_operation(sender, &overlay.session_id, op).await?;
            }
        }
        KeyCode::Char('a') => {
            if let Some(op) = overlay
                .open_widget()
                .or_else(|| {
                    overlay
                        .selected_key()
                        .as_ref()
                        .and_then(|key| overlay.widget(key))
                })
                .and_then(|widget| widget.action.clone())
                && let Some(op) = prepare_overlay_operation(overlay, op)
            {
                submit_operation(sender, &overlay.session_id, op).await?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn move_overlay_selection(overlay: &mut CapabilityOverlay, delta: isize) {
    let option_count = overlay
        .open_widget()
        .and_then(|widget| widget.content.as_ref())
        .and_then(|content| match content {
            FrontendWidgetContent::Picker { options, .. } => Some(options.len()),
            FrontendWidgetContent::ActionList { items, .. } => Some(items.len()),
            FrontendWidgetContent::Blocks { .. } => None,
        });
    if let Some(option_count) = option_count {
        overlay.option_list.select(moved_index(
            overlay.option_list.selected(),
            option_count,
            delta,
        ));
        overlay.action_index = 0;
    } else if overlay.open.is_none() {
        overlay.widget_list.select(moved_index(
            overlay.widget_list.selected(),
            overlay.widgets.len(),
            delta,
        ));
    }
}

fn select_overlay_edge(overlay: &mut CapabilityOverlay, last: bool) {
    let option_count = overlay
        .open_widget()
        .and_then(|widget| widget.content.as_ref())
        .and_then(|content| match content {
            FrontendWidgetContent::Picker { options, .. } => Some(options.len()),
            FrontendWidgetContent::ActionList { items, .. } => Some(items.len()),
            FrontendWidgetContent::Blocks { .. } => None,
        });
    let (list, length) = if let Some(option_count) = option_count {
        (&mut overlay.option_list, option_count)
    } else if overlay.open.is_none() {
        (&mut overlay.widget_list, overlay.widgets.len())
    } else {
        return;
    };
    list.select(
        length
            .checked_sub(1)
            .map(|last_index| if last { last_index } else { 0 }),
    );
    overlay.action_index = 0;
}

fn activate_overlay(overlay: &mut CapabilityOverlay) -> Option<Op> {
    if let Some(widget) = overlay.open_widget() {
        return match widget.content.as_ref() {
            Some(FrontendWidgetContent::Picker { options, .. }) => options
                .get(overlay.option_list.selected().unwrap_or_default())
                .map(|option| option.op.clone()),
            Some(FrontendWidgetContent::ActionList { items, .. }) => items
                .get(overlay.option_list.selected().unwrap_or_default())
                .and_then(|item| item.actions.get(overlay.action_index))
                .map(|action| action.op.clone()),
            _ => widget.action.clone(),
        };
    }
    let key = overlay.selected_key()?;
    let widget = overlay.widget(&key)?;
    let action = widget.action.clone();
    if widget.content.is_some() {
        overlay.open = Some(key);
        overlay.sync_selection();
        action
    } else {
        action
    }
}

fn move_overlay_action(overlay: &mut CapabilityOverlay, delta: isize) {
    let Some(length) = overlay
        .selected_action_list_item()
        .map(|item| item.actions.len())
    else {
        return;
    };
    overlay.action_index =
        moved_index(Some(overlay.action_index), length, delta).unwrap_or_default();
}

fn prepare_overlay_operation(overlay: &mut CapabilityOverlay, op: Op) -> Option<Op> {
    let seed = match &op {
        Op::CapabilityCommand {
            input: Some(input), ..
        } => input.clone(),
        _ => return Some(op),
    };
    let text = truncate_input(terminal_text(&seed));
    overlay.input = Some(ActionInput {
        cursor: text.len(),
        text,
        op,
    });
    None
}

fn handle_action_input_key(overlay: &mut CapabilityOverlay, key: KeyEvent) -> Option<Op> {
    let input = overlay.input.as_mut()?;
    match key.code {
        KeyCode::Esc => {
            overlay.input = None;
            None
        }
        KeyCode::Enter => {
            let mut input = overlay.input.take().expect("action input checked");
            if let Op::CapabilityCommand { input: value, .. } = &mut input.op {
                *value = Some(input.text);
            }
            Some(input.op)
        }
        KeyCode::Backspace => {
            let previous = previous_boundary(&input.text, input.cursor);
            input.text.drain(previous..input.cursor);
            input.cursor = previous;
            None
        }
        KeyCode::Delete => {
            let next = next_boundary(&input.text, input.cursor);
            input.text.drain(input.cursor..next);
            None
        }
        KeyCode::Left => {
            input.cursor = previous_boundary(&input.text, input.cursor);
            None
        }
        KeyCode::Right => {
            input.cursor = next_boundary(&input.text, input.cursor);
            None
        }
        KeyCode::Home => {
            input.cursor = 0;
            None
        }
        KeyCode::End => {
            input.cursor = input.text.len();
            None
        }
        KeyCode::Char(character) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
            insert_action_input(input, &character.to_string());
            None
        }
        _ => None,
    }
}

fn handle_overlay_paste(state: &mut DashboardState, value: &str) {
    if let Some(input) = state
        .overlay
        .as_mut()
        .and_then(|overlay| overlay.input.as_mut())
    {
        insert_action_input(input, value);
    }
}

fn insert_action_input(input: &mut ActionInput, value: &str) {
    let value = terminal_text(value);
    let available = MAX_USER_INPUT_BYTES.saturating_sub(input.text.len());
    let value = truncate_to_bytes(&value, available);
    input.text.insert_str(input.cursor, value);
    input.cursor += value.len();
}

fn truncate_input(mut value: String) -> String {
    value.truncate(truncate_to_bytes(&value, MAX_USER_INPUT_BYTES).len());
    value
}

fn truncate_to_bytes(value: &str, limit: usize) -> &str {
    let mut end = value.len().min(limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn previous_boundary(value: &str, cursor: usize) -> usize {
    value[..cursor]
        .char_indices()
        .next_back()
        .map_or(0, |(index, _)| index)
}

fn next_boundary(value: &str, cursor: usize) -> usize {
    value[cursor..]
        .char_indices()
        .nth(1)
        .map_or(value.len(), |(index, _)| cursor + index)
}

async fn submit_operation(sender: &GatewaySender, session_id: &str, op: Op) -> Result<()> {
    sender
        .send(ClientMessage::Submit {
            session_id: session_id.into(),
            submission: Submission {
                id: Uuid::new_v4().to_string(),
                op,
            },
        })
        .await
        .map_err(gateway_error)
}

fn handle_navigation_key(state: &mut DashboardState, key: KeyEvent, area: Rect) -> bool {
    let areas = dashboard_areas(area);
    let page = match state.focus {
        DashboardFocus::Devices => areas.devices.height.saturating_sub(2),
        DashboardFocus::Chats => areas.chats.height.saturating_sub(2),
    }
    .max(1);
    let page = isize::try_from(page).unwrap_or(isize::MAX);
    match key.code {
        KeyCode::Tab => {
            state.focus = match state.focus {
                DashboardFocus::Devices => DashboardFocus::Chats,
                DashboardFocus::Chats => DashboardFocus::Devices,
            }
        }
        KeyCode::Up | KeyCode::Char('k') => move_selection(state, -1),
        KeyCode::Down | KeyCode::Char('j') => move_selection(state, 1),
        KeyCode::PageUp => move_selection(state, -page),
        KeyCode::PageDown => move_selection(state, page),
        KeyCode::Home => select_edge(state, false),
        KeyCode::End => select_edge(state, true),
        _ => return false,
    }
    true
}

fn handle_mouse(state: &mut DashboardState, mouse: MouseEvent, area: Rect) {
    let areas = dashboard_areas(area);
    let (focus, delta) = match mouse.kind {
        MouseEventKind::ScrollUp if contains(areas.devices, mouse.column, mouse.row) => {
            (DashboardFocus::Devices, -3)
        }
        MouseEventKind::ScrollDown if contains(areas.devices, mouse.column, mouse.row) => {
            (DashboardFocus::Devices, 3)
        }
        MouseEventKind::ScrollUp if contains(areas.chats, mouse.column, mouse.row) => {
            (DashboardFocus::Chats, -3)
        }
        MouseEventKind::ScrollDown if contains(areas.chats, mouse.column, mouse.row) => {
            (DashboardFocus::Chats, 3)
        }
        _ => return,
    };
    state.focus = focus;
    move_selection(state, delta);
}

fn contains(area: Rect, column: u16, row: u16) -> bool {
    column >= area.x && column < area.right() && row >= area.y && row < area.bottom()
}

fn move_selection(state: &mut DashboardState, delta: isize) {
    match state.focus {
        DashboardFocus::Devices => {
            let ordered = ordered_clients(&state.clients);
            let current = state
                .selected_client_id
                .as_deref()
                .and_then(|id| ordered.iter().position(|client| client.client_id == id));
            let selected = moved_index(current, ordered.len(), delta);
            state.selected_client_id = selected.map(|index| ordered[index].client_id.clone());
            state.device_list.select(selected);
        }
        DashboardFocus::Chats => {
            let ordered = ordered_sessions(&state.gateway.sessions);
            let current = state.selected_session_id.as_deref().and_then(|id| {
                ordered
                    .iter()
                    .position(|session| session.summary.session_id == id)
            });
            let selected = moved_index(current, ordered.len(), delta);
            state.selected_session_id =
                selected.map(|index| ordered[index].summary.session_id.clone());
            state.chat_list.select(selected);
        }
    }
}

fn select_edge(state: &mut DashboardState, last: bool) {
    let length = match state.focus {
        DashboardFocus::Devices => state.clients.len(),
        DashboardFocus::Chats => state.gateway.sessions.len(),
    };
    let Some(selected) = length
        .checked_sub(1)
        .map(|last_index| if last { last_index } else { 0 })
    else {
        return;
    };
    match state.focus {
        DashboardFocus::Devices => {
            let ordered = ordered_clients(&state.clients);
            state.selected_client_id = Some(ordered[selected].client_id.clone());
            state.device_list.select(Some(selected));
        }
        DashboardFocus::Chats => {
            let ordered = ordered_sessions(&state.gateway.sessions);
            state.selected_session_id = Some(ordered[selected].summary.session_id.clone());
            state.chat_list.select(Some(selected));
        }
    }
}

fn moved_index(current: Option<usize>, length: usize, delta: isize) -> Option<usize> {
    let last = length.checked_sub(1)?;
    Some(
        current
            .unwrap_or_default()
            .saturating_add_signed(delta)
            .min(last),
    )
}

fn begin_unpair(state: &mut DashboardState) {
    let Some(client) = ordered_clients(&state.clients)
        .into_iter()
        .find(|client| Some(client.client_id.as_str()) == state.selected_client_id.as_deref())
    else {
        return;
    };
    if Some(client.client_id.as_str()) == state.current_client_id.as_deref() {
        state.error = Some("the dashboard cannot unpair its own device".into());
        return;
    }
    state.error = None;
    state.pending_unpair = Some((client.client_id.clone(), client.label.clone()));
}

async fn confirm_unpair(sender: &GatewaySender, state: &mut DashboardState) -> Result<()> {
    let Some((client_id, _)) = state.pending_unpair.take() else {
        return Ok(());
    };
    state.error = None;
    sender
        .send(ClientMessage::UnpairClient {
            request_id: Uuid::new_v4().to_string(),
            client_id,
        })
        .await
        .map_err(gateway_error)
}

async fn request_snapshot(sender: &GatewaySender) -> Result<()> {
    sender
        .send(ClientMessage::ListClients {
            request_id: Uuid::new_v4().to_string(),
        })
        .await
        .map_err(gateway_error)?;
    sender
        .send(ClientMessage::GetProfile {
            request_id: Uuid::new_v4().to_string(),
        })
        .await
        .map_err(gateway_error)
}

fn handle_frame(state: &mut DashboardState, message: ServerMessage) -> Result<()> {
    match message {
        ServerMessage::Ready { payload } | ServerMessage::GatewayConfigured { payload, .. } => {
            state.gateway = payload;
            sync_chat_selection(state);
        }
        ServerMessage::Sessions { sessions, .. } => {
            state.gateway.sessions = sessions;
            sync_chat_selection(state);
        }
        ServerMessage::Clients {
            current_client_id,
            clients,
            ..
        } => {
            state.current_client_id = Some(current_client_id);
            state.clients = clients;
            sync_device_selection(state);
        }
        ServerMessage::Profile { profile, .. } => state.profile = Some(profile),
        ServerMessage::SessionOpened {
            request_id,
            payload,
        } => {
            let Some((pending_request, expected_session)) = state.pending_open.as_ref() else {
                return Ok(());
            };
            if request_id != *pending_request {
                return Ok(());
            }
            if payload.session.session_id != *expected_session {
                state.error = Some("gateway opened a different chat than requested".into());
            } else {
                state.overlay = Some(CapabilityOverlay::from_session(payload));
            }
            state.pending_open = None;
        }
        ServerMessage::AgentEvent { session_id, record } => {
            if let Some(overlay) = state
                .overlay
                .as_mut()
                .filter(|overlay| overlay.session_id == session_id)
                && let EventMsg::Frontend(event) = record.event.msg
            {
                overlay.apply(event);
            }
        }
        ServerMessage::Rejected {
            request_id,
            message,
            fatal,
            ..
        } => {
            if state
                .pending_open
                .as_ref()
                .is_some_and(|(pending, _)| pending == &request_id)
            {
                state.pending_open = None;
            }
            if fatal {
                return Err(Error::Stopped(message));
            }
            state.error = Some(message);
        }
        ServerMessage::Error { message, fatal, .. } => {
            if fatal {
                return Err(Error::Stopped(message));
            }
            state.error = Some(message);
        }
        _ => {}
    }
    Ok(())
}

fn sync_device_selection(state: &mut DashboardState) {
    let ordered = ordered_clients(&state.clients);
    let selected = state
        .selected_client_id
        .as_deref()
        .and_then(|id| ordered.iter().position(|client| client.client_id == id))
        .or_else(|| (!ordered.is_empty()).then_some(0));
    state.selected_client_id = selected.map(|index| ordered[index].client_id.clone());
    state.device_list.select(selected);
}

fn sync_chat_selection(state: &mut DashboardState) {
    let ordered = ordered_sessions(&state.gateway.sessions);
    let selected = state
        .selected_session_id
        .as_deref()
        .and_then(|id| {
            ordered
                .iter()
                .position(|session| session.summary.session_id == id)
        })
        .or_else(|| (!ordered.is_empty()).then_some(0));
    state.selected_session_id = selected.map(|index| ordered[index].summary.session_id.clone());
    state.chat_list.select(selected);
}

fn ordered_clients(clients: &[ClientStatus]) -> Vec<&ClientStatus> {
    let mut clients = clients.iter().collect::<Vec<_>>();
    clients.sort_by(|left, right| {
        (left.connections == 0)
            .cmp(&(right.connections == 0))
            .then_with(|| left.label.cmp(&right.label))
    });
    clients
}

fn ordered_sessions(sessions: &[SessionRecord]) -> Vec<&SessionRecord> {
    let mut sessions = sessions.iter().collect::<Vec<_>>();
    sessions.sort_by_key(|session| session.activity.state == SessionActivityState::Idle);
    sessions
}

fn render(frame: &mut ratatui::Frame<'_>, state: &mut DashboardState) {
    let theme = current();
    frame.render_widget(
        Block::default().style(theme.style(Role::Canvas)),
        frame.area(),
    );
    let areas = dashboard_areas(frame.area());
    render_header(frame, areas.header, state);
    render_devices(frame, areas.devices, state);
    render_chats(frame, areas.chats, state);
    render_providers(frame, areas.providers, state);
    render_defaults(frame, areas.defaults, state);
    render_usage(frame, areas.usage, state.profile.as_ref());
    let (footer, role) = if let Some((_, label)) = &state.pending_unpair {
        (
            format!(" Unpair {}? · y confirm · n cancel ", terminal_text(label)),
            Role::Warning,
        )
    } else if let Some(error) = &state.error {
        (error.clone(), Role::Error)
    } else if state.pending_open.is_some() {
        (" opening chat capabilities… ".into(), Role::Muted)
    } else {
        (
            " tab devices/chats · ↑↓ scroll · enter chat capabilities · u unpair · p provider · d defaults · r refresh · q quit ".into(),
            Role::Muted,
        )
    };
    frame.render_widget(
        Paragraph::new(terminal_text(&footer)).style(theme.style(role)),
        areas.footer,
    );
    if let Some(overlay) = state.overlay.as_mut() {
        render_capability_overlay(frame, overlay);
    }
}

fn render_capability_overlay(frame: &mut ratatui::Frame<'_>, overlay: &mut CapabilityOverlay) {
    let area = centered_area(frame.area(), 86, 82);
    frame.render_widget(Clear, area);
    let outer = panel(format!("Chat capabilities · {}", overlay.session_id), true);
    let inner = outer.inner(area);
    frame.render_widget(outer, area);
    let [body, footer] = Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);
    let footer_text = if let Some(input) = overlay.input.as_ref() {
        render_action_input(frame, body, input);
        " type to edit · enter save · esc cancel "
    } else if overlay.open.is_none() {
        render_navigation_widgets(frame, body, overlay);
        " ↑↓ select · enter open/run · esc close "
    } else {
        let content = overlay
            .open_widget()
            .and_then(|widget| widget.content.clone());
        match content {
            Some(FrontendWidgetContent::Blocks { title, blocks }) => {
                render_blocks(frame, body, &title, &blocks);
                if overlay
                    .open_widget()
                    .is_some_and(|widget| widget.action.is_some())
                {
                    " enter/a run · esc back "
                } else {
                    " esc back "
                }
            }
            Some(FrontendWidgetContent::Picker { title, options }) => {
                render_overlay_picker(frame, body, &title, &options, &mut overlay.option_list);
                " ↑↓ select · enter run · esc back "
            }
            Some(FrontendWidgetContent::ActionList { title, items }) => {
                render_action_list(
                    frame,
                    body,
                    &title,
                    &items,
                    &mut overlay.option_list,
                    overlay.action_index,
                );
                " ↑↓ note · ←→ action · enter run · esc back "
            }
            None => {
                frame.render_widget(Paragraph::new(" No content"), body);
                " esc back "
            }
        }
    };
    frame.render_widget(
        Paragraph::new(footer_text).style(current().style(Role::Muted)),
        footer,
    );
}

fn render_action_input(frame: &mut ratatui::Frame<'_>, area: Rect, input: &ActionInput) {
    let mut value = input.text.clone();
    value.insert(input.cursor, '█');
    frame.render_widget(
        Paragraph::new(value)
            .style(current().style(Role::Text))
            .block(
                Block::bordered()
                    .border_style(current().style(Role::Border))
                    .title(" Edit "),
            )
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn centered_area(area: Rect, width: u16, height: u16) -> Rect {
    let [vertical] = Layout::vertical([Constraint::Percentage(height)])
        .flex(ratatui::layout::Flex::Center)
        .areas(area);
    let [center] = Layout::horizontal([Constraint::Percentage(width)])
        .flex(ratatui::layout::Flex::Center)
        .areas(vertical);
    center
}

fn render_navigation_widgets(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    overlay: &mut CapabilityOverlay,
) {
    let theme = current();
    let lines = if overlay.widgets.is_empty() {
        empty("No capability views")
    } else {
        overlay
            .widgets
            .iter()
            .map(|((capability, _), widget)| {
                Line::from(vec![
                    Span::styled(
                        format!(" {}", terminal_text(&widget.text)),
                        theme
                            .style(tone_role(widget.tone))
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!(" · {}", terminal_text(capability)),
                        theme.style(Role::Muted),
                    ),
                ])
            })
            .collect()
    };
    frame.render_stateful_widget(
        List::new(lines)
            .highlight_symbol("› ")
            .highlight_spacing(HighlightSpacing::Always)
            .highlight_style(Style::default().add_modifier(Modifier::BOLD)),
        area,
        &mut overlay.widget_list,
    );
}

fn render_blocks(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    title: &str,
    blocks: &[FrontendBlock],
) {
    let theme = current();
    let mut lines = vec![Line::styled(
        format!(" {}", terminal_text(title)),
        theme.style(Role::AccentStrong).add_modifier(Modifier::BOLD),
    )];
    for block in blocks {
        if lines.len() > 1 {
            lines.push(Line::default());
        }
        lines.extend(
            terminal_text(&super::block_text(block))
                .lines()
                .map(|line| Line::styled(line.to_owned(), theme.style(tone_role(block.tone)))),
        );
    }
    frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), area);
}

fn render_overlay_picker(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    title: &str,
    options: &[horus::protocol::FrontendPickerOption],
    state: &mut ListState,
) {
    let theme = current();
    let [header, list] = Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(area);
    frame.render_widget(
        Paragraph::new(format!(" {}", terminal_text(title)))
            .style(theme.style(Role::AccentStrong).add_modifier(Modifier::BOLD)),
        header,
    );
    let lines = options
        .iter()
        .map(|option| {
            let detail = if !option.shows_detail || option.detail.is_empty() {
                option.description.clone()
            } else {
                format!("{} · {}", option.description, option.detail)
            };
            Line::from(vec![
                Span::styled(
                    format!(" {}", terminal_text(&option.label)),
                    theme.style(Role::Text).add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(" · {}", terminal_text(&detail)),
                    theme.style(Role::Muted),
                ),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_stateful_widget(
        List::new(lines)
            .highlight_symbol("› ")
            .highlight_spacing(HighlightSpacing::Always)
            .highlight_style(Style::default().add_modifier(Modifier::BOLD)),
        list,
        state,
    );
}

fn render_action_list(
    frame: &mut ratatui::Frame<'_>,
    area: Rect,
    title: &str,
    items: &[FrontendActionListItem],
    state: &mut ListState,
    selected_action: usize,
) {
    let theme = current();
    let [header, list] = Layout::vertical([Constraint::Length(2), Constraint::Min(1)]).areas(area);
    frame.render_widget(
        Paragraph::new(format!(" {}", terminal_text(title)))
            .style(theme.style(Role::AccentStrong).add_modifier(Modifier::BOLD)),
        header,
    );
    if items.is_empty() {
        frame.render_widget(
            Paragraph::new(" No items").style(theme.style(Role::Muted)),
            list,
        );
        return;
    }
    let selected_item = state.selected().unwrap_or_default();
    let width = usize::from(list.width.saturating_sub(3));
    let lines = items
        .iter()
        .enumerate()
        .map(|(item_index, item)| {
            let (marker, note_style) = match item.state {
                FrontendListItemState::Plain => ("", theme.style(Role::Text)),
                FrontendListItemState::Pending => ("○ ", theme.style(Role::Muted)),
                FrontendListItemState::InProgress => ("◉ ", theme.style(Role::AccentStrong)),
                FrontendListItemState::Completed => (
                    "✓ ",
                    theme.style(Role::Muted).add_modifier(Modifier::CROSSED_OUT),
                ),
            };
            let actions_width = item
                .actions
                .iter()
                .map(|action| Line::from(format!("[{}]", terminal_text(&action.label))).width())
                .sum::<usize>()
                .saturating_add(item.actions.len().saturating_sub(1) * 2);
            let note_width = width.saturating_sub(actions_width.saturating_add(1));
            let note = truncate_terminal_width(
                &format!("{marker}{}", terminal_text(&item.text)),
                note_width,
            );
            let used = Line::from(note.as_str())
                .width()
                .saturating_add(actions_width);
            let padding = width.saturating_sub(used).max(1);
            let mut spans = vec![Span::styled(
                format!(" {note}{}", " ".repeat(padding)),
                note_style,
            )];
            for (action_index, action) in item.actions.iter().enumerate() {
                if action_index > 0 {
                    spans.push(Span::raw("  "));
                }
                let style = if item_index == selected_item && action_index == selected_action {
                    theme
                        .style(Role::AccentStrong)
                        .add_modifier(Modifier::BOLD | Modifier::UNDERLINED)
                } else {
                    theme.style(tone_role(action.tone))
                };
                spans.push(Span::styled(
                    format!("[{}]", terminal_text(&action.label)),
                    style,
                ));
            }
            Line::from(spans)
        })
        .collect::<Vec<_>>();
    frame.render_stateful_widget(
        List::new(lines)
            .highlight_symbol("› ")
            .highlight_spacing(HighlightSpacing::Always),
        list,
        state,
    );
}

fn truncate_terminal_width(value: &str, width: usize) -> String {
    if Line::from(value).width() <= width {
        return value.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    let mut clipped = String::new();
    let mut used = 0;
    let content_width = width.saturating_sub(1);
    for character in value.chars() {
        let character_width = Line::from(character.to_string()).width();
        if used + character_width > content_width {
            break;
        }
        clipped.push(character);
        used += character_width;
    }
    clipped.push('…');
    clipped
}

const fn tone_role(tone: FrontendTone) -> Role {
    match tone {
        FrontendTone::Neutral => Role::Neutral,
        FrontendTone::Success => Role::Success,
        FrontendTone::Warning => Role::Warning,
        FrontendTone::Error => Role::Error,
    }
}

fn dashboard_areas(area: Rect) -> DashboardAreas {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(3),
        Constraint::Min(12),
        Constraint::Length(2),
    ])
    .areas(area);
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(body);
    let [devices, chats] =
        Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(left);
    let [providers, defaults, usage] = Layout::vertical([
        Constraint::Percentage(34),
        Constraint::Percentage(38),
        Constraint::Percentage(28),
    ])
    .areas(right);
    DashboardAreas {
        header,
        devices,
        chats,
        providers,
        defaults,
        usage,
        footer,
    }
}

fn render_header(frame: &mut ratatui::Frame<'_>, area: Rect, state: &DashboardState) {
    let theme = current();
    let active_devices = state
        .clients
        .iter()
        .filter(|client| client.connections > 0)
        .count();
    let active_chats = state
        .gateway
        .sessions
        .iter()
        .filter(|session| session.activity.state != SessionActivityState::Idle)
        .count();
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled(
                    " HORUS GATEWAY ",
                    theme.style(Role::AccentStrong).add_modifier(Modifier::BOLD),
                ),
                Span::styled(&state.endpoint, theme.style(Role::Muted)),
            ]),
            Line::styled(
                format!(
                    " {active_devices}/{} devices active · {active_chats}/{} chats active",
                    state.clients.len(),
                    state.gateway.sessions.len()
                ),
                theme.style(Role::Text),
            ),
        ]),
        area,
    );
}

fn panel(title: impl Into<String>, focused: bool) -> Block<'static> {
    let theme = current();
    Block::default()
        .title(format!(" {} ", title.into()))
        .title_style(theme.style(Role::AccentStrong).add_modifier(Modifier::BOLD))
        .borders(Borders::ALL)
        .border_style(theme.style(if focused {
            Role::AccentStrong
        } else {
            Role::Neutral
        }))
        .style(theme.style(Role::Canvas))
}

fn render_devices(frame: &mut ratatui::Frame<'_>, area: Rect, state: &mut DashboardState) {
    let theme = current();
    let clients = ordered_clients(&state.clients);
    let lines = if clients.is_empty() {
        empty("No paired devices")
    } else {
        clients
            .into_iter()
            .map(|client| {
                let (symbol, status, role) = match client.connections {
                    0 => ("○", "offline".into(), Role::Muted),
                    1 => ("●", "active".into(), Role::Success),
                    connections => ("●", format!("{connections} connections"), Role::Success),
                };
                let kinds = if client.kinds.is_empty() {
                    "paired".into()
                } else {
                    client
                        .kinds
                        .iter()
                        .map(|kind| client_kind(*kind))
                        .collect::<Vec<_>>()
                        .join(" + ")
                };
                let current =
                    if Some(client.client_id.as_str()) == state.current_client_id.as_deref() {
                        " · this device"
                    } else {
                        ""
                    };
                Line::styled(
                    format!(
                        " {symbol} {kinds} · {} · {status}{current}",
                        terminal_text(&client.label)
                    ),
                    theme.style(role),
                )
            })
            .collect()
    };
    frame.render_stateful_widget(
        List::new(lines)
            .block(panel("Devices", state.focus == DashboardFocus::Devices))
            .highlight_symbol("› ")
            .highlight_spacing(HighlightSpacing::Always)
            .highlight_style(Style::default().add_modifier(Modifier::BOLD))
            .scroll_padding(1),
        area,
        &mut state.device_list,
    );
}

fn render_chats(frame: &mut ratatui::Frame<'_>, area: Rect, state: &mut DashboardState) {
    let theme = current();
    let sessions = ordered_sessions(&state.gateway.sessions);
    let lines = if sessions.is_empty() {
        empty("No chats")
    } else {
        sessions
            .into_iter()
            .map(|session| {
                let title = session
                    .title
                    .as_deref()
                    .or(session.summary.first_user_message.as_deref())
                    .unwrap_or(&session.summary.session_id);
                let (symbol, role) = match session.activity.state {
                    SessionActivityState::Idle => ("○", Role::Muted),
                    SessionActivityState::Running => ("●", Role::Success),
                    SessionActivityState::AwaitingApproval => ("●", Role::Warning),
                };
                Line::styled(
                    format!(
                        " {symbol} {} · {}",
                        activity_label(session.activity.state),
                        terminal_text(title)
                    ),
                    theme.style(role),
                )
            })
            .collect()
    };
    frame.render_stateful_widget(
        List::new(lines)
            .block(panel("Chats", state.focus == DashboardFocus::Chats))
            .highlight_symbol("› ")
            .highlight_spacing(HighlightSpacing::Always)
            .highlight_style(Style::default().add_modifier(Modifier::BOLD))
            .scroll_padding(1),
        area,
        &mut state.chat_list,
    );
}

fn render_providers(frame: &mut ratatui::Frame<'_>, area: Rect, state: &DashboardState) {
    let configured = state
        .gateway
        .providers
        .iter()
        .filter_map(|status| {
            status
                .selection
                .as_ref()
                .map(|selection| (status, selection))
        })
        .collect::<Vec<_>>();
    let lines = if configured.is_empty() {
        empty("No providers configured · press p")
    } else {
        configured
            .into_iter()
            .map(|(status, selection)| {
                Line::from(format!(
                    " ● {} · {}",
                    terminal_text(&status.label),
                    terminal_text(&selection.model)
                ))
            })
            .collect()
    };
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel("Providers", false))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_defaults(frame: &mut ratatui::Frame<'_>, area: Rect, state: &DashboardState) {
    let lines = state.gateway.default_config.as_ref().map_or_else(
        || empty("No defaults · configure a provider first"),
        |default| {
            let config = &default.config;
            let enabled = state
                .gateway
                .middleware_features
                .iter()
                .filter(|feature| feature.required || config.middleware.enabled(&feature.id))
                .count();
            vec![
                Line::from(format!(
                    " Model      {} / {}",
                    terminal_text(&config.provider.provider),
                    terminal_text(&config.provider.model)
                )),
                Line::from(format!(
                    " Reasoning  {}",
                    config
                        .provider
                        .reasoning_effort
                        .as_deref()
                        .unwrap_or("provider default")
                )),
                Line::from(format!(" Search     {:?}", config.provider.web_search)),
                Line::from(format!(" Middleware {enabled}")),
            ]
        },
    );
    frame.render_widget(
        Paragraph::new(lines)
            .block(panel("Defaults · d to change", false))
            .wrap(Wrap { trim: true }),
        area,
    );
}

fn render_usage(frame: &mut ratatui::Frame<'_>, area: Rect, profile: Option<&ProfileSnapshot>) {
    let lines = profile.map_or_else(
        || empty("Loading usage…"),
        |profile| {
            let today = current_unix_day()
                .map_or(0_i64, |day| token_total_for_day(&profile.daily_usage, day));
            let total = profile.daily_usage.iter().fold(0_i64, |total, entry| {
                total.saturating_add(entry.usage.total_tokens)
            });
            let stats = &profile.run_stats;
            vec![
                Line::from(format!(" Today      {} tokens", number(today))),
                Line::from(format!(
                    " Runs       {} · {} failed · {} aborted",
                    number(stats.run_count),
                    number(stats.failed_run_count),
                    number(stats.aborted_run_count)
                )),
                Line::from(format!(
                    " Calls      {} model · {} tool · {} failed",
                    number(stats.model_calls),
                    number(stats.tool_calls),
                    number(stats.failed_tool_calls)
                )),
                Line::from(format!(" Run time   {}", elapsed_ms(stats.elapsed_ms))),
                Line::from(format!(" 364 days   {} tokens", number(total))),
            ]
        },
    );
    frame.render_widget(Paragraph::new(lines).block(panel("Usage", false)), area);
}

fn token_total_for_day(usage: &[DailyUsage], unix_day: u64) -> i64 {
    usage
        .iter()
        .filter(|entry| entry.unix_day == unix_day)
        .fold(0_i64, |total, entry| {
            total.saturating_add(entry.usage.total_tokens)
        })
}

fn elapsed_ms(milliseconds: u64) -> String {
    let seconds = milliseconds / 1_000;
    if seconds < 60 {
        format!("{seconds}s")
    } else if seconds < 3_600 {
        format!("{}m {:02}s", seconds / 60, seconds % 60)
    } else {
        format!("{}h {:02}m", seconds / 3_600, seconds / 60 % 60)
    }
}

fn empty(message: &str) -> Vec<Line<'static>> {
    vec![Line::styled(
        format!(" {}", terminal_text(message)),
        current().style(Role::Muted),
    )]
}

const fn client_kind(kind: ClientKind) -> &'static str {
    match kind {
        ClientKind::Cli => "CLI",
        ClientKind::Macos => "macOS",
        ClientKind::Ios => "iOS",
        ClientKind::Ipados => "iPadOS",
        ClientKind::GatewayDashboard => "Dashboard",
    }
}

const fn activity_label(state: SessionActivityState) -> &'static str {
    match state {
        SessionActivityState::Idle => "idle",
        SessionActivityState::Running => "running",
        SessionActivityState::AwaitingApproval => "approval",
    }
}

fn current_unix_day() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs() / 86_400)
}

fn number(value: impl ToString) -> String {
    let digits = value.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, character) in digits.chars().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            formatted.push(',');
        }
        formatted.push(character);
    }
    formatted
}

fn gateway_error(error: horus_gateway::Error) -> Error {
    Error::Stopped(error.to_string())
}

#[cfg(test)]
mod tests {
    use horus::backend::checkpoint::SessionSummary;
    use horus::protocol::{
        FrontendAction, FrontendActionListItem, FrontendBlock, FrontendBlockFormat,
        FrontendBlockRole, FrontendBlockState, FrontendBlockUpdate, FrontendEvent,
        FrontendListItemState, FrontendPickerOption, FrontendSlot, FrontendSymbol, FrontendTone,
        FrontendWidget, FrontendWidgetContent, Op, SessionContext, TokenUsage,
    };
    use horus_gateway::wire::{DailyUsage, SessionActivity, SessionActivityState, SessionRecord};
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
