//! Horus terminal frontend.

use horus::Result;
use horus_gateway::client::{GatewayEvents, GatewaySender};
use horus_gateway::wire::{ReadyPayload, SessionReadyPayload};

mod catalog;
mod gateway;
mod gateway_actions;
mod headless;
mod setup;
mod terminal;
mod theme;
mod tui;

pub(crate) use headless::run as run_headless;
pub(crate) use tui::terminal_text;

pub(crate) async fn run(
    sender: GatewaySender,
    events: GatewayEvents,
    gateway: &mut ReadyPayload,
    session: &mut SessionReadyPayload,
    local_gateway: bool,
    gateway_endpoint: String,
) -> Result<(FrontendExit, GatewaySender, GatewayEvents)> {
    let workspace = session.workspace.path.clone();
    let catalog = catalog::UiCatalog::build(&session.contributions, &gateway.models, &workspace)?;
    tui::runtime::run(
        sender,
        events,
        gateway,
        session,
        catalog,
        local_gateway,
        gateway_endpoint,
    )
    .await
}

/// Why a frontend returned control to its launcher.
pub(crate) enum FrontendExit {
    Exit,
    Discard,
    New,
    Resume(String),
    Reload,
    Reconnect,
}
