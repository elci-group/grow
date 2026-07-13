//! `grow` — user-facing CLI for the GROW runtime.

use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use grow_git::Repository;
use grow_runtime::{Request, Runtime};

#[derive(Parser)]
#[command(
    name = "grow",
    version,
    about = "GROW — Git Repository Oriented Web runtime",
    long_about = "GROW (Git Repository Oriented Web runtime) executes applications \
published as git repositories in a sandboxed WebAssembly environment. \
Applications are served locally instead of being deployed to a server."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Print the verified application identity and runtime configuration
    ///
    /// Loads the repository, verifies the HEAD commit, and displays the
    /// manifest configuration including runtime engine, entry point, and
    /// permissions.
    Inspect {
        /// Path to the GROW repository
        repo: PathBuf,
    },
    /// Execute the repository's WASM entry once and print the response
    ///
    /// Runs the application's WASM module in a fresh sandboxed instance
    /// with a GET / request and prints the response status, headers, and body.
    Run {
        /// Path to the GROW repository
        repo: PathBuf,
    },
    /// Start the local daemon serving the repository on localhost
    ///
    /// Serves static assets from the repository's verified HEAD tree and
    /// routes non-asset requests to the WASM entry module. The server runs
    /// until interrupted (Ctrl+C).
    Serve {
        /// Path to the GROW repository
        repo: PathBuf,
        /// Localhost port to listen on (default: 7870)
        #[arg(short, long, default_value_t = grow_server::DEFAULT_PORT)]
        port: u16,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    match Cli::parse().command {
        Command::Inspect { repo } => inspect(&repo)?,
        // WASM invocation must not run on a tokio worker thread: WASI stdio
        // pipes block the current thread internally.
        Command::Run { repo } => run_once(&repo)?,
        Command::Serve { repo, port } => {
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(grow_server::serve(grow_server::ServeConfig { repo, port }))?;
        }
    }
    Ok(())
}

fn inspect(repo_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(repo_path)
        .map_err(|e| format!("failed to open repository '{}': {}", repo_path.display(), e))?;
    let manifest = repo.manifest();
    println!("\x1b[1;36mname\x1b[0m        {}", manifest.grow.name);
    println!("\x1b[1;36mversion\x1b[0m     {}", manifest.grow.version);
    println!("\x1b[1;36mcommit\x1b[0m      {}", repo.head_commit());
    println!(
        "\x1b[1;36mruntime\x1b[0m     {}",
        manifest.wasm_entry().unwrap_or("\x1b[33mstatic\x1b[0m")
    );
    println!(
        "\x1b[1;36mnetwork\x1b[0m     {}",
        manifest.network.enabled.unwrap_or(false)
    );
    println!(
        "\x1b[1;36mpermissions\x1b[0m filesystem={:?}, camera={:?}, microphone={:?}",
        manifest.permissions.filesystem,
        manifest.permissions.camera,
        manifest.permissions.microphone
    );
    Ok(())
}

fn run_once(repo_path: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(repo_path)?;
    let manifest = repo.manifest();
    let entry = manifest
        .wasm_entry()
        .ok_or("repository manifest declares no WASM runtime entry")?;
    let wasm = repo.read_file(entry)?;
    let runtime = Runtime::new(&wasm)?;
    let response = runtime.invoke(&Request {
        method: "GET".into(),
        path: "/".into(),
        headers: Vec::new(),
        body: Vec::new(),
    })?;

    println!("status: {} {}", response.status, response.reason);
    for (name, value) in &response.headers {
        println!("{name}: {value}");
    }
    println!();
    std::io::stdout().write_all(&response.body)?;
    Ok(())
}
