mod config;
mod frontend;

use frontend::FrontendExit;
use horus::Result;
use horus::agent::create_agent;
use uuid::Uuid;

#[tokio::main]
async fn main() -> Result<()> {
    let workspace = std::env::current_dir()?;
    let mut built = frontend::setup::load_agent_config(&workspace).await?;
    loop {
        let agent = create_agent(built.config.clone()).await?;
        match frontend::run(agent, &workspace).await? {
            FrontendExit::Exit => return Ok(()),
            FrontendExit::New(model_route) => {
                built.config = built
                    .config
                    .clone()
                    .model_route(&model_route, None)?
                    .session_id(Uuid::new_v4().to_string());
            }
            FrontendExit::Resume(session_id) => {
                built.config = built.config.clone().session_id(session_id);
            }
            FrontendExit::Setup => {
                frontend::setup::add_provider(&workspace).await?;
                built = frontend::setup::load_agent_config(&workspace).await?;
            }
        }
    }
}
