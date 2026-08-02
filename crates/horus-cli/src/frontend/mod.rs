//! Horus terminal frontend.

use std::path::PathBuf;

use horus::{Error, Result};
use horus_gateway::client::{GatewayEvents, GatewaySender};
use horus_gateway::wire::ReadyPayload;

mod catalog;
mod gateway_actions;
mod headless;
mod terminal;
mod theme;
mod tui;

pub(crate) use catalog::{CronAction, GatewayAction};
pub(crate) use headless::run as run_headless;
pub(crate) use tui::terminal_text;

pub(crate) async fn execute_gateway_action(
    sender: &GatewaySender,
    events: &mut GatewayEvents,
    ready: &ReadyPayload,
    action: GatewayAction,
) -> Result<String> {
    match gateway_actions::prepare(action, ready)? {
        gateway_actions::PreparedAction::Print(message) => Ok(message),
        gateway_actions::PreparedAction::Send {
            message,
            request_id,
            response,
        } => {
            sender
                .send(message)
                .await
                .map_err(|error| Error::Stopped(error.to_string()))?;
            loop {
                let frame = events
                    .next()
                    .await
                    .map_err(|error| Error::Stopped(error.to_string()))?
                    .ok_or_else(|| Error::Stopped("gateway disconnected".into()))?;
                match &frame.message {
                    horus_gateway::wire::ServerMessage::Rejected {
                        request_id: actual,
                        message,
                        ..
                    } if actual == &request_id => return Err(Error::Stopped(message.clone())),
                    horus_gateway::wire::ServerMessage::Error { message, .. } => {
                        return Err(Error::Stopped(message.clone()));
                    }
                    _ => {}
                }
                if let Some(message) =
                    gateway_actions::render_terminal_response(&frame.message, &request_id, response)
                {
                    return Ok(message);
                }
            }
        }
    }
}

pub(crate) async fn run(
    sender: GatewaySender,
    events: GatewayEvents,
    ready: ReadyPayload,
    local_gateway: bool,
) -> Result<(FrontendExit, GatewaySender, GatewayEvents)> {
    let workspace = PathBuf::from(&ready.workspace.label);
    let catalog =
        catalog::UiCatalog::build(&ready.contributions, &ready.model_choices, &workspace)?;
    tui::runtime::run(sender, events, ready, catalog, local_gateway).await
}

/// Why a frontend returned control to its launcher.
pub(crate) enum FrontendExit {
    Exit,
    New(String),
    Resume(String),
    Reload(Box<ReadyPayload>),
}
