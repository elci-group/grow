//! `grow` — user-facing CLI for the GROW runtime.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use clap::{Parser, Subcommand};
use grow_git::Repository;
use grow_runtime::{Request, Runtime, StorageGrant};
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
    /// Installed apps run under their approval record and storage grant;
    /// plain repository paths run in dev mode (no approval check, no storage).
    Run {
        /// Path to the GROW repository, or installed app name
        app: String,
    },
    /// Start the local daemon serving the repository on localhost
    ///
    /// Serves static assets from the repository's verified HEAD tree and
    /// routes non-asset requests to the WASM entry module. The server runs
    /// until interrupted (Ctrl+C). Approval and storage handling match run.
    Serve {
        /// Path to the GROW repository, or installed app name
        app: String,
        /// Localhost port to listen on (default: 7870)
        #[arg(short, long, default_value_t = grow_server::DEFAULT_PORT)]
        port: u16,
    },
    /// Install a GROW repository into the user app registry
    ///
    /// Records the verified commit, manifest identity, source path, requested
    /// capabilities, and an explicit approval marker for declared host powers.
    /// The source is a local repository path or a file:// URL; cloned sources
    /// are kept in the registry cache until the app is removed.
    Install {
        /// Path to the GROW repository, or a file:// URL to clone
        repo: String,
        /// Approve declared host capabilities for this verified app state
        #[arg(long)]
        approve_capabilities: bool,
    },
    /// List installed GROW apps
    List,
    /// Remove an installed GROW app from the local registry
    ///
    /// Deletes the app's registry metadata and storage, plus the cached
    /// clone when the app was installed from a clone source. External source
    /// repositories are never touched.
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

    // Print errors in plain Display form; `main`'s Result return would use
    // the noisier Debug representation.
    if let Err(err) = dispatch(Cli::parse().command) {
        eprintln!("error: {err}");
        std::process::exit(1);
    }
    Ok(())
}

fn dispatch(command: Command) -> Result<(), Box<dyn std::error::Error>> {
    match command {
        Command::Inspect { app } => inspect(&resolve_app(&app)?.repo_path)?,
        // WASM invocation must not run on a tokio worker thread: WASI stdio
        // pipes block the current thread internally.
        Command::Run { app } => {
            let root = registry_root()?;
            run_once(&root, &resolve_in(&root, &app)?)?;
        }
        Command::Serve { app, port } => {
            let root = registry_root()?;
            let resolved = resolve_in(&root, &app)?;
            let repo = Repository::open(&resolved.repo_path)?;
            let storage = enforce_install_policy(&root, &resolved, &repo)?;
            let runtime = tokio::runtime::Runtime::new()?;
            runtime.block_on(grow_server::serve(grow_server::ServeConfig {
                repo: resolved.repo_path,
                port,
                storage,
            }))?;
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
    approval: ApprovalRecord,
    requested: RequestedCapabilities,
}

#[derive(Debug, Deserialize, Serialize)]
struct ApprovalRecord {
    app: String,
    commit: String,
    approved: bool,
    approved_at_unix: Option<u64>,
    capabilities: Vec<String>,
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

/// Result of resolving a CLI app argument: the repository to load plus the
/// registry record when the argument named an installed app. `installed` is
/// `None` for plain repository paths, which run in dev mode (no approval
/// gate, no storage grant).
#[derive(Debug)]
struct ResolvedApp {
    repo_path: PathBuf,
    installed: Option<InstalledApp>,
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

fn run_once(root: &Path, resolved: &ResolvedApp) -> Result<(), Box<dyn std::error::Error>> {
    let repo = Repository::open(&resolved.repo_path)?;
    let storage = enforce_install_policy(root, resolved, &repo)?;
    let manifest = repo.manifest();
    let entry = manifest
        .wasm_entry()
        .ok_or("repository manifest declares no WASM runtime entry")?;
    let wasm = repo.read_file(entry)?;
    let runtime = Runtime::new(&wasm)?;
    let response = runtime.invoke(
        &Request {
            method: "GET".into(),
            path: "/".into(),
            headers: Vec::new(),
            body: Vec::new(),
        },
        storage.as_ref(),
    )?;

    println!("status: {} {}", response.status, response.reason);
    for (name, value) in &response.headers {
        println!("{name}: {value}");
    }
    println!();
    std::io::stdout().write_all(&response.body)?;
    Ok(())
}

/// Enforce the install-time approval record against the repository's
/// verified HEAD and build the app's storage grant. Path-based resolution is
/// dev mode: no approval check and no storage grant. Approved host powers
/// are only *recorded* — modules receive no host capabilities yet beyond the
/// storage directory below.
fn enforce_install_policy(
    root: &Path,
    resolved: &ResolvedApp,
    repo: &Repository,
) -> Result<Option<StorageGrant>, Box<dyn std::error::Error>> {
    let Some(app) = &resolved.installed else {
        return Ok(None);
    };
    let head = repo.head_commit();
    if head != app.commit {
        return Err(format!(
            "app '{}' has changed since install (was {}, now {}); run grow install again to re-verify",
            app.name,
            short_commit(&app.commit),
            short_commit(head),
        )
        .into());
    }
    if app.requested.has_host_powers() && !app.approval.approved {
        return Err(format!(
            "app '{}' declares host capabilities that are not approved; reinstall with --approve-capabilities",
            app.name
        )
        .into());
    }
    let dir = app_dir_in(root, &app.name)?.join("storage");
    Ok(Some(StorageGrant::new(dir, repo.manifest().quota_bytes()?)))
}

fn install(source: &str, approve_capabilities: bool) -> Result<(), Box<dyn std::error::Error>> {
    install_into(&registry_root()?, source, approve_capabilities)
}

fn install_into(
    root: &Path,
    source: &str,
    approve_capabilities: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let path = PathBuf::from(source);
    let repo = if path.exists() {
        Repository::open(&path)
            .map_err(|e| format!("failed to open repository '{}': {}", path.display(), e))?
    } else {
        clone_into_cache(root, source)?
    };
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

    let app_dir = app_dir_in(root, &manifest.grow.name)?;
    fs::create_dir_all(app_dir.join("storage"))?;
    let commit = repo.head_commit().to_string();
    let approval = ApprovalRecord {
        app: manifest.grow.name.clone(),
        commit: commit.clone(),
        approved: approve_capabilities || !requested.has_host_powers(),
        approved_at_unix: approve_capabilities
            .then(current_unix_timestamp)
            .transpose()?,
        capabilities: requested.capability_names(),
    };

    let installed = InstalledApp {
        name: manifest.grow.name.clone(),
        version: manifest.grow.version.clone(),
        commit,
        repo_path: repo.root().to_path_buf(),
        runtime: manifest.wasm_entry().unwrap_or("static").to_string(),
        storage_quota: manifest.storage.quota.clone(),
        approval,
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

/// Unique staging directory under the registry cache for an in-flight clone.
fn staging_dir(cache_root: &Path) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    cache_root.join(format!(
        ".staging-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ))
}

/// Clone `source` (a local path or `file://` URL) into the registry cache,
/// learn the app name from the cloned manifest, and keep the clone at
/// `<root>/cache/<name>/`, replacing any previous cache for that app.
fn clone_into_cache(root: &Path, source: &str) -> Result<Repository, Box<dyn std::error::Error>> {
    let cache_root = root.join("cache");
    fs::create_dir_all(&cache_root)?;
    let staging = staging_dir(&cache_root);
    let _ = fs::remove_dir_all(&staging);
    let staged = match Repository::clone_from(source, &staging) {
        Ok(repo) => repo,
        Err(err) => {
            let _ = fs::remove_dir_all(&staging);
            return Err(err.into());
        }
    };
    let name = staged.manifest().grow.name.clone();
    if !is_valid_app_name(&name) {
        let _ = fs::remove_dir_all(&staging);
        return Err(
            format!("invalid app name '{name}'; use letters, numbers, '.', '_' or '-'").into(),
        );
    }
    let dest = cache_root.join(&name);
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    fs::rename(&staging, &dest)?;
    Ok(Repository::open(&dest)?)
}

fn list_installed() -> Result<(), Box<dyn std::error::Error>> {
    list_in(&registry_root()?)
}

fn list_in(root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let apps = installed_apps_in(root)?;
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

fn installed_apps_in(root: &Path) -> Result<Vec<InstalledApp>, Box<dyn std::error::Error>> {
    let apps_dir = root.join("apps");
    let mut apps = Vec::new();
    if apps_dir.is_dir() {
        for entry in fs::read_dir(apps_dir)? {
            let metadata = entry?.path().join("metadata.toml");
            if metadata.is_file() {
                apps.push(read_installed_metadata(&metadata)?);
            }
        }
    }
    apps.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(apps)
}

fn remove_installed(app: &str) -> Result<(), Box<dyn std::error::Error>> {
    remove_from(&registry_root()?, app)
}

fn remove_from(root: &Path, app: &str) -> Result<(), Box<dyn std::error::Error>> {
    let app_dir = app_dir_in(root, app)?;
    if !app_dir.exists() {
        return Err(format!("app '{app}' is not installed").into());
    }
    // A repo recorded inside the registry root is a registry-owned cache
    // clone: delete it with the app. External source repos are never touched.
    let metadata = app_dir.join("metadata.toml");
    if metadata.is_file() {
        let installed = read_installed_metadata(&metadata)?;
        let root = root.canonicalize().unwrap_or_else(|_| root.to_path_buf());
        if installed.repo_path != root
            && installed.repo_path.starts_with(&root)
            && installed.repo_path.exists()
        {
            fs::remove_dir_all(&installed.repo_path)?;
        }
    }
    fs::remove_dir_all(&app_dir)?;
    println!("removed {app}");
    Ok(())
}

fn resolve_app(input: &str) -> Result<ResolvedApp, Box<dyn std::error::Error>> {
    let path = PathBuf::from(input);
    if path.exists() {
        return Ok(ResolvedApp {
            repo_path: path,
            installed: None,
        });
    }
    resolve_in(&registry_root()?, input)
}

fn resolve_in(root: &Path, input: &str) -> Result<ResolvedApp, Box<dyn std::error::Error>> {
    let path = PathBuf::from(input);
    if path.exists() {
        return Ok(ResolvedApp {
            repo_path: path,
            installed: None,
        });
    }

    let metadata = app_dir_in(root, input)?.join("metadata.toml");
    if metadata.is_file() {
        let app = read_installed_metadata(&metadata)?;
        return Ok(ResolvedApp {
            repo_path: app.repo_path.clone(),
            installed: Some(app),
        });
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
        return Err(
            format!("invalid app name '{name}'; use letters, numbers, '.', '_' or '-'").into(),
        );
    }
    Ok(root.join("apps").join(name))
}

fn is_valid_app_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

fn short_commit(commit: &str) -> &str {
    commit.get(..12).unwrap_or(commit)
}

fn current_unix_timestamp() -> Result<u64, Box<dyn std::error::Error>> {
    Ok(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs())
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

    fn capability_names(&self) -> Vec<String> {
        let mut names = Vec::new();
        if self.network {
            names.push("network".into());
        }
        if let Some(filesystem) = &self.filesystem {
            names.push(format!("filesystem:{filesystem}"));
        }
        if self.camera {
            names.push("camera".into());
        }
        if self.microphone {
            names.push("microphone".into());
        }
        if self.clipboard {
            names.push("clipboard".into());
        }
        if self.notifications {
            names.push("notifications".into());
        }
        names
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    /// WASI module that echoes the request back after a fixed response head
    /// (adapted from grow-runtime's ECHO_WAT fixture).
    const ECHO_WAT: &str = r#"(module
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (param i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "200 OK\0aContent-Type: text/plain\0a\0agrow wasm says: ")
  (func (export "_start")
    (i32.store (i32.const 1024) (i32.const 256))
    (i32.store (i32.const 1028) (i32.const 512))
    (drop (call $fd_read (i32.const 0) (i32.const 1024) (i32.const 1) (i32.const 1032)))
    (i32.store (i32.const 1040) (i32.const 0))
    (i32.store (i32.const 1044) (i32.const 49))
    (drop (call $fd_write (i32.const 1) (i32.const 1040) (i32.const 1) (i32.const 1048)))
    (i32.store (i32.const 1056) (i32.const 256))
    (i32.store (i32.const 1060) (i32.load (i32.const 1032)))
    (drop (call $fd_write (i32.const 1) (i32.const 1056) (i32.const 1) (i32.const 1064)))
    (call $proc_exit (i32.const 0))
  )
)"#;

    const MANIFEST: &str = r#"
[grow]
name = "fixture-app"
version = "0.1.0"

[runtime]
engine = "wasm"
entry = "main.wasm"
"#;

    const CAMERA_MANIFEST: &str = r#"
[grow]
name = "fixture-app"
version = "0.1.0"

[runtime]
engine = "wasm"
entry = "main.wasm"

[permissions]
camera = true
"#;

    const QUOTA_MANIFEST: &str = r#"
[grow]
name = "fixture-app"
version = "0.1.0"

[runtime]
engine = "wasm"
entry = "main.wasm"

[storage]
quota = "1mb"
"#;

    fn git(dir: &Path, args: &[&str]) {
        let out = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("git runs");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    fn fixture_repo(manifest: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        git(dir.path(), &["init", "-b", "main"]);
        fs::create_dir_all(dir.path().join(".grow")).unwrap();
        fs::write(dir.path().join(".grow/manifest.toml"), manifest).unwrap();
        fs::write(
            dir.path().join("main.wasm"),
            wat::parse_str(ECHO_WAT).unwrap(),
        )
        .unwrap();
        git(dir.path(), &["add", "-A"]);
        git(
            dir.path(),
            &[
                "-c",
                "user.email=fixture@grow.test",
                "-c",
                "user.name=grow-fixture",
                "commit",
                "-m",
                "fixture",
            ],
        );
        dir
    }

    fn install_from_path(root: &Path, repo: &Path) {
        install_into(root, repo.to_str().unwrap(), false).expect("install succeeds");
    }

    #[test]
    fn validates_app_names() {
        assert!(is_valid_app_name("hello-app"));
        assert!(is_valid_app_name("hello.app_1"));
        assert!(!is_valid_app_name(""));
        assert!(!is_valid_app_name("."));
        assert!(!is_valid_app_name(".."));
        assert!(!is_valid_app_name("../hello"));
        assert!(!is_valid_app_name("hello/app"));
    }

    #[test]
    fn detects_host_power_requests() {
        assert!(!RequestedCapabilities::default().has_host_powers());
        assert!(
            RequestedCapabilities {
                network: true,
                ..RequestedCapabilities::default()
            }
            .has_host_powers()
        );
        assert!(
            RequestedCapabilities {
                filesystem: Some("full".into()),
                ..RequestedCapabilities::default()
            }
            .has_host_powers()
        );
        assert!(
            !RequestedCapabilities {
                filesystem: Some("restricted".into()),
                ..RequestedCapabilities::default()
            }
            .has_host_powers()
        );
    }

    #[test]
    fn lists_declared_capabilities() {
        let requested = RequestedCapabilities {
            network: true,
            filesystem: Some("restricted".into()),
            camera: true,
            microphone: false,
            clipboard: true,
            notifications: false,
        };

        assert_eq!(
            requested.capability_names(),
            vec!["network", "filesystem:restricted", "camera", "clipboard"]
        );
    }

    #[test]
    fn install_list_run_remove_lifecycle() {
        let root = tempfile::tempdir().unwrap();
        let repo = fixture_repo(MANIFEST);

        install_from_path(root.path(), repo.path());
        let apps = installed_apps_in(root.path()).unwrap();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "fixture-app");

        let resolved = resolve_in(root.path(), "fixture-app").unwrap();
        assert!(resolved.installed.is_some());
        run_once(root.path(), &resolved).expect("run by name succeeds");

        remove_from(root.path(), "fixture-app").unwrap();
        assert!(installed_apps_in(root.path()).unwrap().is_empty());
        let err = resolve_in(root.path(), "fixture-app").unwrap_err();
        assert!(
            err.to_string()
                .contains("neither a path nor an installed GROW app"),
            "got: {err}"
        );
    }

    #[test]
    fn host_powers_require_approval_flag() {
        let root = tempfile::tempdir().unwrap();
        let repo = fixture_repo(CAMERA_MANIFEST);

        let err = install_into(root.path(), repo.path().to_str().unwrap(), false).unwrap_err();
        assert!(
            err.to_string()
                .contains("rerun with --approve-capabilities"),
            "got: {err}"
        );

        install_into(root.path(), repo.path().to_str().unwrap(), true).expect("install succeeds");
        let resolved = resolve_in(root.path(), "fixture-app").unwrap();
        run_once(root.path(), &resolved).expect("run by name succeeds");
    }

    #[test]
    fn unapproved_installed_app_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let repo = fixture_repo(CAMERA_MANIFEST);
        install_into(root.path(), repo.path().to_str().unwrap(), true).unwrap();

        // Simulate an install record written before approval was granted.
        let metadata = root.path().join("apps/fixture-app/metadata.toml");
        let mut app = read_installed_metadata(&metadata).unwrap();
        app.approval.approved = false;
        fs::write(&metadata, toml::to_string_pretty(&app).unwrap()).unwrap();

        let resolved = resolve_in(root.path(), "fixture-app").unwrap();
        let err = run_once(root.path(), &resolved).unwrap_err();
        assert_eq!(
            err.to_string(),
            "app 'fixture-app' declares host capabilities that are not approved; \
             reinstall with --approve-capabilities"
        );
    }

    #[test]
    fn commit_drift_requires_reinstall() {
        let root = tempfile::tempdir().unwrap();
        let repo = fixture_repo(MANIFEST);
        install_from_path(root.path(), repo.path());

        git(
            repo.path(),
            &[
                "-c",
                "user.email=fixture@grow.test",
                "-c",
                "user.name=grow-fixture",
                "commit",
                "--amend",
                "-m",
                "changed",
            ],
        );

        let resolved = resolve_in(root.path(), "fixture-app").unwrap();
        let err = run_once(root.path(), &resolved).unwrap_err();
        let text = err.to_string();
        assert!(
            text.contains("app 'fixture-app' has changed since install"),
            "got: {text}"
        );
        assert!(
            text.contains("run grow install again to re-verify"),
            "got: {text}"
        );

        // Dev mode: running by plain path skips the approval gate.
        let dev = resolve_in(root.path(), repo.path().to_str().unwrap()).unwrap();
        assert!(dev.installed.is_none());
        run_once(root.path(), &dev).expect("path-based run skips the gate");
    }

    #[test]
    fn unsupported_remote_scheme_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let err = install_into(root.path(), "https://example.com/repo.git", false).unwrap_err();
        assert!(
            err.to_string()
                .contains("remote URL scheme is not supported"),
            "got: {err}"
        );
    }

    #[test]
    fn file_url_install_uses_registry_cache() {
        let root = tempfile::tempdir().unwrap();
        let src = fixture_repo(MANIFEST);
        let url = format!("file://{}", src.path().display());

        install_into(root.path(), &url, false).expect("install from file:// URL");
        let resolved = resolve_in(root.path(), "fixture-app").unwrap();
        run_once(root.path(), &resolved).expect("run from cached clone");

        let app = resolved.installed.as_ref().expect("installed app");
        let cache = root
            .path()
            .canonicalize()
            .unwrap()
            .join("cache/fixture-app");
        assert_eq!(app.repo_path, cache);
        assert!(cache.join(".grow/manifest.toml").is_file());

        remove_from(root.path(), "fixture-app").unwrap();
        assert!(!cache.exists(), "cached clone is deleted");
        assert!(installed_apps_in(root.path()).unwrap().is_empty());
    }

    #[test]
    fn installed_app_storage_dir_is_created_and_used() {
        let root = tempfile::tempdir().unwrap();
        let repo = fixture_repo(QUOTA_MANIFEST);
        install_from_path(root.path(), repo.path());

        let storage = root.path().join("apps/fixture-app/storage");
        assert!(storage.is_dir(), "grant directory exists after install");

        let resolved = resolve_in(root.path(), "fixture-app").unwrap();
        run_once(root.path(), &resolved).expect("run with quota grant succeeds");
    }
}
