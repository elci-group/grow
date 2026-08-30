//! grow-mobile: the Grow Browser engine for Android.
//!
//! Embeds the real grow runtime on device: `grow://` URLs are parsed with
//! the shared [`grow_server::resolve`] grammar, repositories are cloned over
//! https (pure-Rust gix, no host git binary), the HEAD commit and manifest
//! are verified exactly like the desktop daemon, and the site is served
//! static-only (`grow-server` without the `wasm` feature) on a per-app
//! localhost port that a WebView navigates to.
//!
//! The engine is callable from Kotlin through JNI ([`DevGrowBrowserEngine`]
//! symbol set, built for `target_os = "android"`) and from tests as a plain
//! Rust library. Resolution results cross the JNI boundary as JSON.

use std::path::{Path, PathBuf};

use grow_git::Repository;
use grow_server::resolve::{GrowTarget, Resolver, parse_grow_url};
use serde::Serialize;

/// Result of resolving a `grow://` URL, serialized to JSON for the Kotlin
/// browser layer.
#[derive(Debug, Serialize)]
pub struct ResolveResult {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ResolveResult {
    fn err(message: String) -> Self {
        Self {
            ok: false,
            url: None,
            app: None,
            commit: None,
            port: None,
            error: Some(message),
        }
    }
}

/// The browser engine: owns the registry directory, the tokio runtime the
/// per-app servers run on, and the serving bookkeeping.
pub struct Engine {
    root: PathBuf,
    resolver: Resolver,
    runtime: tokio::runtime::Runtime,
}

impl Engine {
    /// Start the engine with its registry under `root` (the app's files
    /// directory on Android). Repositories are installed at
    /// `<root>/<host>/<owner>/<repo>` on first use.
    pub fn start(root: &Path) -> Result<Self, String> {
        std::fs::create_dir_all(root).map_err(|e| format!("cannot create registry dir: {e}"))?;
        let runtime = tokio::runtime::Runtime::new().map_err(|e| e.to_string())?;
        Ok(Self {
            root: root.to_path_buf(),
            resolver: Resolver::new(),
            runtime,
        })
    }

    /// Resolve a `grow://` URL: parse, install on first use, serve, and
    /// return the localhost URL to navigate to.
    pub fn resolve(&self, url: &str) -> ResolveResult {
        match parse_grow_url(url) {
            Ok(target) => {
                let source = target.clone_source();
                self.resolve_target(&target, &source)
            }
            Err(err) => ResolveResult::err(err),
        }
    }

    /// Resolve an already-parsed target, cloning from `source` on first use.
    /// Split from [`Engine::resolve`] so tests can point the engine at a
    /// local `file://` fixture instead of the network.
    pub fn resolve_target(&self, target: &GrowTarget, source: &str) -> ResolveResult {
        match self.resolve_inner(target, source) {
            Ok(result) => result,
            Err(err) => ResolveResult::err(err),
        }
    }

    fn resolve_inner(&self, target: &GrowTarget, source: &str) -> Result<ResolveResult, String> {
        let repo_dir = self
            .root
            .join(&target.host)
            .join(&target.owner)
            .join(&target.repo);

        // Install on first use only: like the desktop daemon, repos are
        // never auto-updated — the verified commit keeps serving until the
        // app is explicitly reinstalled.
        if !repo_dir.join(".git").exists() {
            // gix (unlike the git CLI) does not create intermediate
            // directories for the clone destination.
            if let Some(parent) = repo_dir.parent() {
                std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
            }
            Repository::clone_from(source, &repo_dir).map_err(|e| e.to_string())?;
        }
        let repo = Repository::open(&repo_dir).map_err(|e| e.to_string())?;
        let app = repo.manifest().grow.name.clone();
        let commit = repo.head_commit().to_string();
        drop(repo);

        let state = grow_server::load(&repo_dir).map_err(|e| e.to_string())?;
        let port = self
            .runtime
            .block_on(self.resolver.serve_app(&app, state))?;
        Ok(ResolveResult {
            ok: true,
            url: Some(format!("http://127.0.0.1:{port}{}", target.path)),
            app: Some(app),
            commit: Some(commit),
            port: Some(port),
            error: None,
        })
    }

    /// Snapshot of `(app, port)` pairs currently served.
    pub fn apps(&self) -> Vec<(String, u16)> {
        self.resolver.apps()
    }
}

#[cfg(target_os = "android")]
mod jni_bridge {
    use std::path::Path;
    use std::sync::OnceLock;

    use jni::JNIEnv;
    use jni::objects::{JClass, JString};
    use jni::sys::jstring;

    use super::Engine;

    /// The engine lives for the process lifetime; per-app server tasks keep
    /// running on its tokio runtime.
    static ENGINE: OnceLock<Engine> = OnceLock::new();

    fn to_java(env: &mut JNIEnv, payload: String) -> jstring {
        match env.new_string(payload) {
            Ok(s) => s.into_raw(),
            Err(_) => std::ptr::null_mut(),
        }
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_grow_browser_Engine_nativeStart(
        mut env: JNIEnv,
        _class: JClass,
        files_dir: JString,
    ) -> jstring {
        let dir: String = env
            .get_string(&files_dir)
            .map(|s| s.into())
            .unwrap_or_default();
        let payload = if ENGINE.get().is_some() {
            r#"{"ok":true}"#.to_string()
        } else {
            match Engine::start(Path::new(&dir).join("registry").as_path()) {
                Ok(engine) => match ENGINE.set(engine) {
                    Ok(()) => r#"{"ok":true}"#.to_string(),
                    Err(_) => r#"{"ok":true}"#.to_string(), // raced start; engine is up
                },
                Err(err) => serde_json::json!({ "ok": false, "error": err }).to_string(),
            }
        };
        to_java(&mut env, payload)
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_grow_browser_Engine_nativeResolve(
        mut env: JNIEnv,
        _class: JClass,
        url: JString,
    ) -> jstring {
        let url: String = env.get_string(&url).map(|s| s.into()).unwrap_or_default();
        let payload = match ENGINE.get() {
            Some(engine) => serde_json::to_string(&engine.resolve(&url))
                .unwrap_or_else(|e| format!(r#"{{"ok":false,"error":"{e}"}}"#)),
            None => r#"{"ok":false,"error":"engine not started"}"#.to_string(),
        };
        to_java(&mut env, payload)
    }

    #[unsafe(no_mangle)]
    pub extern "system" fn Java_dev_grow_browser_Engine_nativeApps(
        mut env: JNIEnv,
        _class: JClass,
    ) -> jstring {
        let payload = match ENGINE.get() {
            Some(engine) => {
                let apps: Vec<serde_json::Value> = engine
                    .apps()
                    .iter()
                    .map(|(app, port)| {
                        serde_json::json!({
                            "app": app,
                            "port": port,
                            "url": format!("http://127.0.0.1:{port}/"),
                        })
                    })
                    .collect();
                serde_json::json!({ "apps": apps }).to_string()
            }
            None => r#"{"apps":[]}"#.to_string(),
        };
        to_java(&mut env, payload)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::process::Command;

    const MANIFEST: &str = r#"
[grow]
name = "mob-fixture"
version = "0.1.0"

[network]
enabled = false
"#;

    fn fixture_repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        let git = |args: &[&str]| {
            let out = Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .output()
                .expect("git runs");
            assert!(
                out.status.success(),
                "git {args:?} failed: {}",
                String::from_utf8_lossy(&out.stderr)
            );
        };
        git(&["init", "-b", "main"]);
        std::fs::create_dir_all(dir.path().join(".grow")).unwrap();
        std::fs::write(dir.path().join(".grow/manifest.toml"), MANIFEST).unwrap();
        std::fs::write(
            dir.path().join("index.html"),
            "<!doctype html><h1>hello mobile</h1>",
        )
        .unwrap();
        git(&["add", "-A"]);
        git(&[
            "-c",
            "user.email=fixture@grow.test",
            "-c",
            "user.name=grow-fixture",
            "commit",
            "-m",
            "fixture",
        ]);
        dir
    }

    fn http_get(url: &str) -> String {
        let addr = url
            .trim_start_matches("http://")
            .split('/')
            .next()
            .unwrap()
            .to_string();
        let path = format!(
            "/{}",
            url.trim_start_matches("http://")
                .split_once('/')
                .map(|(_, rest)| rest)
                .unwrap_or("")
        );
        let mut stream = TcpStream::connect(&addr).expect("connects");
        stream
            .write_all(format!("GET {path} HTTP/1.0\r\nHost: {addr}\r\n\r\n").as_bytes())
            .unwrap();
        let mut buf = String::new();
        stream.read_to_string(&mut buf).unwrap();
        buf
    }

    #[test]
    fn resolves_file_backed_repo_end_to_end() {
        let src = fixture_repo();
        let root = tempfile::tempdir().unwrap();
        let engine = Engine::start(root.path()).unwrap();
        let target = GrowTarget {
            host: "github.com".into(),
            owner: "o".into(),
            repo: "r".into(),
            path: "/".into(),
        };
        let source = format!("file://{}", src.path().display());

        let result = engine.resolve_target(&target, &source);
        assert!(result.ok, "resolve failed: {:?}", result.error);
        assert_eq!(result.app.as_deref(), Some("mob-fixture"));
        assert_eq!(result.commit.as_ref().map(String::len), Some(40));

        let body = http_get(result.url.as_ref().unwrap());
        assert!(body.contains("hello mobile"), "body: {body}");

        // Second resolve serves the already-installed app on the same port.
        let again = engine.resolve_target(&target, &source);
        assert!(again.ok);
        assert_eq!(again.port, result.port);

        let apps = engine.apps();
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].0, "mob-fixture");
    }

    #[test]
    fn bad_url_reports_error_not_panic() {
        let root = tempfile::tempdir().unwrap();
        let engine = Engine::start(root.path()).unwrap();
        let result = engine.resolve("grow://example.com/o/r");
        assert!(!result.ok);
        assert!(result.error.unwrap().contains("not supported"));
    }
}
