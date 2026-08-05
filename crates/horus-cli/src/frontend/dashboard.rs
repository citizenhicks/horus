//! Live gateway dashboard and gateway-scoped setup entrypoints.

use std::env;
use std::io;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use horus::backend::sandbox::ApprovalPolicy;
use horus::{Error, Result};
use horus_gateway::client::{Endpoint, GatewayClient, GatewayEvents, GatewaySender};
use horus_gateway::config::{ConfigStore, GatewayConfig};
use horus_gateway::wire::{
    ClientKind, ClientMessage, ClientStatus, ProfileSnapshot, ReadyPayload, ServerMessage,
    SessionActivityState,
};
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui::crossterm::event::{
    Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseEvent, MouseEventKind,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, HighlightSpacing, List, ListState, Paragraph, Wrap};
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
    device_list: ListState,
    chat_list: ListState,
    focus: DashboardFocus,
    pending_unpair: Option<(String, String)>,
    profile: Option<ProfileSnapshot>,
    error: Option<String>,
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
            device_list: ListState::default(),
            chat_list: ListState::default(),
            focus: DashboardFocus::Devices,
            pending_unpair: None,
            profile: None,
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
            let selected = moved_index(
                state.chat_list.selected(),
                state.gateway.sessions.len(),
                delta,
            );
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
        DashboardFocus::Chats => state.chat_list.select(Some(selected)),
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
        ServerMessage::Rejected { message, fatal, .. }
        | ServerMessage::Error { message, fatal, .. } => {
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
    let selected = state
        .gateway
        .sessions
        .len()
        .checked_sub(1)
        .map(|last| state.chat_list.selected().unwrap_or_default().min(last));
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
    } else {
        (
            " tab devices/chats · ↑↓ scroll · pgup/pgdn · u unpair · p provider · d defaults · r refresh · q quit ".into(),
            Role::Muted,
        )
    };
    frame.render_widget(
        Paragraph::new(terminal_text(&footer)).style(theme.style(role)),
        areas.footer,
    );
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
    let mut sessions = state.gateway.sessions.iter().collect::<Vec<_>>();
    sessions.sort_by_key(|session| session.activity.state == SessionActivityState::Idle);
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
                Line::from(format!(" Approval   {}", approval_label(config.approval))),
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
                .and_then(|day| {
                    profile
                        .daily_usage
                        .iter()
                        .find(|entry| entry.unix_day == day)
                })
                .map(|entry| &entry.usage)
                .cloned()
                .unwrap_or_default();
            let total = profile.daily_usage.iter().fold(0_i64, |total, entry| {
                total.saturating_add(entry.usage.total_tokens)
            });
            vec![
                Line::from(format!(" Today      {} tokens", number(today.total_tokens))),
                Line::from(format!(
                    " Input/out  {} / {}",
                    number(today.input_tokens),
                    number(today.output_tokens)
                )),
                Line::from(format!(" 364 days   {} tokens", number(total))),
            ]
        },
    );
    frame.render_widget(Paragraph::new(lines).block(panel("Usage", false)), area);
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

const fn approval_label(policy: ApprovalPolicy) -> &'static str {
    match policy {
        ApprovalPolicy::On => "ask",
        ApprovalPolicy::Allow => "allow · no network",
        ApprovalPolicy::AllowNetwork => "allow · network",
    }
}

fn current_unix_day() -> Option<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs() / 86_400)
}

fn number(value: i64) -> String {
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
    use super::moved_index;

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
}
