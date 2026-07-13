//! grow-server daemon binary.

use std::path::PathBuf;

use clap::Parser;

#[derive(Parser)]
#[command(
    name = "grow-server",
    version,
    about = "GROW local orchestration daemon"
)]
struct Args {
    /// Path to the GROW repository to serve
    repo: PathBuf,
    /// Localhost port to listen on
    #[arg(short, long, default_value_t = grow_server::DEFAULT_PORT)]
    port: u16,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    grow_server::serve(grow_server::ServeConfig {
        repo: args.repo,
        port: args.port,
    })
    .await?;
    Ok(())
}
