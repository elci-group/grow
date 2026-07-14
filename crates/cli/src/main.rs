//! `grow` — user-facing CLI for the GROW runtime.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use grow_git::Repository;
use grow_runtime::{Request, Runtime};
use serde::{Deserialize, Serialize};

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
        /// Path to the GROW repository, or installed app name
        app: String,
    },
    /// Execute the repository's WASM entry once and print the response
    ///
    /// Runs the application's WASM module in a fresh sandboxed instance
    /// with a GET / request and prints the response status, headers, and body.
    Run {
        /// Path to the GROW repository, or installed app name
        app: String,
    },
    /// Start the local daemon serving the repository on localhost
    ///
    /// Serves static assets from the repository's verified HEAD tree and
    /// routes non-asset requests to the WASM entry module. The server runs
    /// until interrupted (Ctrl+C).
    Serve {
        /// Path to the GROW repository, or installed app name
        app: String,
        /// Localhost port to listen on (default: 7870)
        #[arg(short, long, default_value_t = grow_server::DEFAULT_PORT)]
        port: u16,
    },
    /// Install a local GROW repository into the user app registry
    ///
    /// Records the verified commit, manifest identity, source path, requested
    /// capabilities, and an explicit approval marker for declared host powers.
    Install {
        /// Path to the GROW repository
        repo: PathBuf,
        /// Approve declared host capabilities for this verified app state
        #[arg(long)]
        approve_capabilities: bool,
    },
    /// List installed GROW apps
    List,
    /// Remove an installed GROW app from the local registry
    Remove {
        /// Installed app name
        app: String,
    },
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    match Cli::parse().command {
        Command::Inspect { app } => inspect(&resolve_app(&app)?)?,
        // WASM invocation must not run on a tokio worker thread: WASI stdio
        // pipes block the current thread internally.
        Command::Run { app } => run_once(&resolve_app(&app)?)?,
        Command::Serve { app, port } => {
            let runtime = tokio::runtime::Runtime::new()?;
            let repo = resolve_app(&app)?;
            runtime.block_on(grow_server::serve(grow_server::ServeConfig { repo, port }))?;
        }
        Command::Install {
            repo,
            approve_capabilities,
        } => install(&repo, approve_capabilities)?,
        Command::List => list_installed()?,
        Command::Remove { app } => remove_installed(&app)?,
    }
    Ok(())
}

#[derive(Debug, Deserialize, Serialize)]
struct InstalledApp {
    name: String,
    version: String,
    commit: String,
    repo_path: PathBuf,
    runtime: String,
    storage_quota: Option<String>,
    capabilities_approved: bool,
    requested: RequestedCapabilities,
}

#[derive(Debug, Default, Deserialize, Serialize)]
struct RequestedCapabilities {
    network: bool,
    filesystem: Option<String>,
    camera: bool,
    microphone: bool,
    clipboard: bool,
    notifications: bool,
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
    println!(
        "\x1b[1;36mstorage\x1b[0m     {}",
        manifest.storage.quota.as_deref().unwrap_or("unlimited")
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

fn install(repo_path: &Path, approve_capabilities: bool) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(repo_path)
        .map_err(|e| format!("failed to open repository '{}': {}", repo_path.display(), e))?;
    let manifest = repo.manifest();
    let requested = RequestedCapabilities {
        network: manifest.network.enabled.unwrap_or(false),
        filesystem: manifest.permissions.filesystem.clone(),
        camera: manifest.permissions.camera.unwrap_or(false),
        microphone: manifest.permissions.microphone.unwrap_or(false),
        clipboard: manifest.permissions.clipboard.unwrap_or(false),
        notifications: manifest.permissions.notifications.unwrap_or(false),
    };

    if requested.has_host_powers() && !approve_capabilities {
        return Err(format!(
            "app '{}' requests host capabilities; rerun with --approve-capabilities to record approval",
            manifest.grow.name
        )
        .into());
    }

    let root = registry_root()?;
    let app_dir = app_dir_in(&root, &manifest.grow.name)?;
    fs::create_dir_all(app_dir.join("storage"))?;

    let installed = InstalledApp {
        name: manifest.grow.name.clone(),
        version: manifest.grow.version.clone(),
        commit: repo.head_commit().to_string(),
        repo_path: repo.root().to_path_buf(),
        runtime: manifest.wasm_entry().unwrap_or("static").to_string(),
        storage_quota: manifest.storage.quota.clone(),
        capabilities_approved: approve_capabilities,
        requested,
    };

    let text = toml::to_string_pretty(&installed)?;
    fs::write(app_dir.join("metadata.toml"), text)?;
    println!(
        "installed {} {} at {}",
        installed.name,
        installed.version,
        app_dir.display()
    );
    Ok(())
}

fn list_installed() -> Result<(), Box<dyn std::error::Error>> {
    let root = registry_root()?;
    let apps_dir = root.join("apps");
    if !apps_dir.exists() {
        println!("no installed apps");
        return Ok(());
    }

    let mut apps = Vec::new();
    for entry in fs::read_dir(apps_dir)? {
        let entry = entry?;
        let metadata = entry.path().join("metadata.toml");
        if metadata.is_file() {
            apps.push(read_installed_metadata(&metadata)?);
        }
    }
    apps.sort_by(|a, b| a.name.cmp(&b.name));

    if apps.is_empty() {
        println!("no installed apps");
        return Ok(());
    }

    for app in apps {
        println!(
            "{} {} {} {}",
            app.name,
            app.version,
            short_commit(&app.commit),
            app.repo_path.display()
        );
    }
    Ok(())
}

fn remove_installed(app: &str) -> Result<(), Box<dyn std::error::Error>> {
    let root = registry_root()?;
    let app_dir = app_dir_in(&root, app)?;
    if !app_dir.exists() {
        return Err(format!("app '{app}' is not installed").into());
    }
    fs::remove_dir_all(&app_dir)?;
    println!("removed {app}");
    Ok(())
}

fn resolve_app(input: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let path = PathBuf::from(input);
    if path.exists() {
        return Ok(path);
    }

    let root = registry_root()?;
    let metadata = app_dir_in(&root, input)?.join("metadata.toml");
    if metadata.is_file() {
        return Ok(read_installed_metadata(&metadata)?.repo_path);
    }

    Err(format!("'{input}' is neither a path nor an installed GROW app").into())
}

fn read_installed_metadata(path: &Path) -> Result<InstalledApp, Box<dyn std::error::Error>> {
    let text = fs::read_to_string(path)?;
    Ok(toml::from_str(&text)?)
}

fn registry_root() -> Result<PathBuf, Box<dyn std::error::Error>> {
    if let Some(path) = std::env::var_os("GROW_HOME") {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("XDG_DATA_HOME") {
        return Ok(PathBuf::from(path).join("grow"));
    }
    if let Some(path) = std::env::var_os("HOME") {
        return Ok(PathBuf::from(path).join(".local/share/grow"));
    }
    Err("cannot determine app registry path; set GROW_HOME".into())
}

fn app_dir_in(root: &Path, name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    if !is_valid_app_name(name) {
        return Err(format!(
            "invalid app name '{name}'; use letters, numbers, '.', '_' or '-'"
        )
        .into());
    }
    Ok(root.join("apps").join(name))
}

fn is_valid_app_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn short_commit(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

impl RequestedCapabilities {
    fn has_host_powers(&self) -> bool {
        self.network
            || self.camera
            || self.microphone
            || self.clipboard
            || self.notifications
            || matches!(self.filesystem.as_deref(), Some("full"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_app_names() {
        assert!(is_valid_app_name("hello-app"));
        assert!(is_valid_app_name("hello.app_1"));
        assert!(!is_valid_app_name(""));
        assert!(!is_valid_app_name("../hello"));
        assert!(!is_valid_app_name("hello/app"));
    }

    #[test]
    fn detects_host_power_requests() {
        assert!(!RequestedCapabilities::default().has_host_powers());
        assert!(RequestedCapabilities {
            network: true,
            ..RequestedCapabilities::default()
        }
        .has_host_powers());
        assert!(RequestedCapabilities {
            filesystem: Some("full".into()),
            ..RequestedCapabilities::default()
        }
        .has_host_powers());
        assert!(!RequestedCapabilities {
            filesystem: Some("restricted".into()),
            ..RequestedCapabilities::default()
        }
        .has_host_powers());
    }
}
