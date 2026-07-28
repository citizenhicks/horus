//! Horus terminal frontend.

use std::path::Path;

use horus::Result;
use horus::agent::Agent;

mod catalog;
pub(crate) mod setup;
mod terminal;
mod theme;
mod tui;

pub(crate) async fn run(agent: Agent, workspace: &Path) -> Result<FrontendExit> {
    let catalog = catalog::UiCatalog::build(
        agent.frontend().contributions(),
        agent.model_choices(),
        workspace,
    )?;
    tui::runtime::run(agent, catalog).await
}

/// Why a frontend returned control to its launcher.
pub(crate) enum FrontendExit {
    Exit,
    New(String),
    Resume(String),
    Setup,
}
