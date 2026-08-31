//! grow-server daemon binary.
mod curly_expand;


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
async fn __curly_original_main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let args = Args::parse();
    grow_server::serve(grow_server::ServeConfig {
        repo: args.repo,
        port: args.port,
        #[cfg(feature = "wasm")]
        storage: None,
    })
    .await?;
    Ok(())
}

fn main() {
    let raw_args: Vec<String> = std::env::args().collect();
    let mut positions: Vec<usize> = Vec::new();
    let mut fields: Vec<Vec<String>> = Vec::new();
    if let Some(__v) = raw_args.get(1) {
        if !__v.starts_with('-') {
            positions.push(1);
            fields.push(curly_expand::expand_or_literal(__v));
        }
    }

    if fields.is_empty() || fields.iter().all(|f| f.len() <= 1) {
        __curly_original_main();
        return;
    }

    let combos = curly_expand::cartesian(&fields);
    let exe = std::env::current_exe().expect("resolve current exe");
    let mut had_failure = false;
    for combo in &combos {
        let mut new_args = raw_args.clone();
        for (slot, value) in positions.iter().zip(combo.iter()) {
            new_args[*slot] = value.clone();
        }
        let status = std::process::Command::new(&exe)
            .args(&new_args[1..])
            .status()
            .expect("failed to re-exec self");
        if !status.success() {
            had_failure = true;
        }
    }
    if had_failure {
        std::process::exit(1);
    }
}
