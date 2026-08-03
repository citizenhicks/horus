use horus_gateway::Result;

#[tokio::main]
async fn main() -> Result<()> {
    horus_gateway::command::run(std::env::args_os().skip(1).collect()).await
}
