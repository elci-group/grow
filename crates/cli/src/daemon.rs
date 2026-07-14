//! `grow daemon` — the `grow://` resolver control plane for the browser
//! extension.
//!
//! The extension cannot speak to git or spawn processes, so it asks this
//! daemon over plain localhost HTTP. `GET /resolve?u=grow://host/owner/repo[/path]`
//! installs the app on first use (https clone from github.com or gitlab.com,
//! gated by [`crate::install_into`]), serves it on a per-app localhost port
//! with the same approval/storage policy as `grow serve`, and answers with
//! the URL the browser should navigate to.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::extract::{Query, Request, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use tokio::net::TcpListener;

use crate::{enforce_install_policy, install_into, installed_apps_in, resolve_in, InstalledApp};

/// Default localhost port for the control plane.
pub const DEFAULT_PORT: u16 = 7890;

/// Apps are served on sequential ports starting here, skipping any port
/// already taken by another process.
const FIRST_APP_PORT: u16 = 7901;

type BoxError = Box<dyn std::error::Error>;

#[derive(Clone)]
struct DaemonState {
    root: PathBuf,
    served: Arc<Mutex<HashMap<String, u16>>>,
}

/// Run the control plane on `127.0.0.1:port` until interrupted.
pub fn run(root: &Path, port: u16) -> Result<(), BoxError> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        let state = DaemonState {
            root: root.to_path_buf(),
            served: Arc::new(Mutex::new(HashMap::new())),
        };
        let router = Router::new()
            .route("/", get(index))
            .route("/healthz", get(healthz))
            .route("/apps", get(apps))
            .route("/resolve", get(resolve))
            .layer(middleware::from_fn(cors_headers))
            .with_state(state);
        let addr = std::net::SocketAddr::from(([127, 0, 0, 1], port));
        let listener = TcpListener::bind(addr).await?;
        tracing::info!("grow:// resolver listening on http://{addr}");
        axum::serve(listener, router).await?;
        Ok(())
    })
}

/// The extension's origin is a `chrome-extension://` URL, so cross-origin
/// responses must be explicitly allowed. GETs here are simple requests; the
/// resolver only ever serves localhost.
async fn cors_headers(request: Request, next: Next) -> Response {
    let mut response = next.run(request).await;
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response
}

async fn healthz() -> &'static str {
    "ok\n"
}

async fn index(State(state): State<DaemonState>) -> Html<String> {
    let mut rows = String::new();
    let served = state.served.lock().unwrap().clone();
    let mut apps: Vec<(String, u16)> = served.into_iter().collect();
    apps.sort();
    for (app, port) in apps {
        rows.push_str(&format!(
            "<li><a href=\"http://127.0.0.1:{port}/\">{app}</a> — port {port}</li>\n"
        ));
    }
    if rows.is_empty() {
        rows.push_str("<li><em>no apps served yet</em></li>\n");
    }
    Html(format!(
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>grow daemon</title>\
         <style>body{{font-family:ui-monospace,monospace;margin:2rem;background:#0d1411;color:#e9f2ed}}\
         a{{color:#5cb584}}#error{{display:none;background:#3d1a1a;border:1px solid #7a3030;\
         padding:.8rem 1rem;border-radius:8px;margin:1rem 0;color:#f0b8b8}}</style></head><body>\
         <h1>grow:// resolver</h1>\
         <div id=\"error\"></div>\
         <p>Resolve: <code>/resolve?u=grow://github.com/&lt;owner&gt;/&lt;repo&gt;</code></p>\
         <h2>served apps</h2><ul>{rows}</ul>\
         <script>const p=new URLSearchParams(location.hash.slice(1));\
         const e=p.get('error');if(e){{const d=document.getElementById('error');\
         d.style.display='block';\
         d.textContent='Could not resolve '+(p.get('u')||'')+': '+e;}}</script>\
         </body></html>"
    ))
}

async fn apps(State(state): State<DaemonState>) -> Json<serde_json::Value> {
    let served = state.served.lock().unwrap().clone();
    let apps: Vec<serde_json::Value> = served
        .iter()
        .map(|(app, port)| {
            serde_json::json!({ "app": app, "port": port, "url": format!("http://127.0.0.1:{port}/") })
        })
        .collect();
    Json(serde_json::json!({ "apps": apps }))
}

#[derive(Deserialize)]
struct ResolveQuery {
    /// The grow:// URL to resolve.
    u: String,
}

async fn resolve(State(state): State<DaemonState>, Query(query): Query<ResolveQuery>) -> Response {
    match resolve_grow_url(&state, &query.u).await {
        Ok(info) => Json(info).into_response(),
        Err(err) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": err })),
        )
            .into_response(),
    }
}

/// A parsed `grow://host/owner/repo[/path...]` URL.
#[derive(Debug, PartialEq)]
struct GrowTarget {
    host: String,
    owner: String,
    repo: String,
    /// Path within the served site, always rooted (`/` when absent).
    path: String,
}

impl GrowTarget {
    /// The https clone source for this repository.
    fn clone_source(&self) -> String {
        format!("https://{}/{}/{}.git", self.host, self.owner, self.repo)
    }
}

/// Parse a `grow://` URL. The first two path segments are the owner and
/// repository (gitlab subgroups are not supported yet); any remaining
/// segments become the path navigated to within the served site.
fn parse_grow_url(url: &str) -> Result<GrowTarget, String> {
    let rest = url
        .strip_prefix("grow://")
        .ok_or("URL must start with grow://")?;
    let rest = rest.trim_matches('/');
    let mut segments = rest.split('/');
    let host = segments.next().unwrap_or_default();
    let host = host.strip_prefix("www.").unwrap_or(host);
    if !matches!(host, "github.com" | "gitlab.com") {
        return Err(format!(
            "host '{host}' is not supported; use grow://github.com/... or grow://gitlab.com/..."
        ));
    }
    let segment = |name: &str, value: Option<&str>| -> Result<String, String> {
        let value = value.unwrap_or_default();
        if value.is_empty()
            || !value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        {
            return Err(format!("invalid {name} in grow:// URL: '{value}'"));
        }
        Ok(value.trim_end_matches(".git").to_string())
    };
    let owner = segment("owner", segments.next())?;
    let repo = segment("repo", segments.next())?;
    let inner: Vec<&str> = segments.collect();
    let path = if inner.is_empty() {
        "/".to_string()
    } else {
        let joined = inner.join("/");
        if !grow_core::is_safe_relative_path(&joined) {
            return Err(format!("unsafe path in grow:// URL: '{joined}'"));
        }
        format!("/{joined}")
    };
    Ok(GrowTarget {
        host: host.to_string(),
        owner,
        repo,
        path,
    })
}

/// Resolve a grow:// URL to a running app server, installing and starting
/// the app when needed.
async fn resolve_grow_url(
    state: &DaemonState,
    url: &str,
) -> Result<serde_json::Value, String> {
    let target = parse_grow_url(url)?;
    let source = target.clone_source();

    let root = state.root.clone();
    let source_for_install = source.clone();
    let app = tokio::task::spawn_blocking(move || -> Result<InstalledApp, String> {
        let already = installed_apps_in(&root)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|app| app.source.as_deref() == Some(source_for_install.as_str()));
        if let Some(app) = already {
            return Ok(app);
        }
        install_into(&root, &source_for_install, false).map_err(|e| e.to_string())?;
        installed_apps_in(&root)
            .map_err(|e| e.to_string())?
            .into_iter()
            .find(|app| app.source.as_deref() == Some(source_for_install.as_str()))
            .ok_or_else(|| format!("install of '{source_for_install}' did not register"))
    })
    .await
    .map_err(|e| e.to_string())??;

    let port = ensure_served(state, &app).await?;
    Ok(serde_json::json!({
        "url": format!("http://127.0.0.1:{port}{}", target.path),
        "app": app.name,
        "version": app.version,
        "commit": app.commit,
        "port": port,
        "path": target.path,
    }))
}

/// Serve the app on its own localhost port, starting a server task on first
/// use. The approval record and storage grant are enforced exactly as in
/// `grow serve`.
async fn ensure_served(state: &DaemonState, app: &InstalledApp) -> Result<u16, String> {
    if let Some(port) = state.served.lock().unwrap().get(&app.name) {
        return Ok(*port);
    }

    let resolved = resolve_in(&state.root, &app.name).map_err(|e| e.to_string())?;
    let repo = grow_git::Repository::open(&resolved.repo_path).map_err(|e| e.to_string())?;
    let storage = enforce_install_policy(&state.root, &resolved, &repo).map_err(|e| e.to_string())?;
    let app_state = grow_server::load_with_storage(&resolved.repo_path, storage)
        .map_err(|e| e.to_string())?;
    let router = grow_server::app(app_state);

    let mut port = *state.served.lock().unwrap().values().max().unwrap_or(&(FIRST_APP_PORT - 1)) + 1;
    let listener = loop {
        match TcpListener::bind(std::net::SocketAddr::from(([127, 0, 0, 1], port))).await {
            Ok(listener) => break listener,
            Err(_) if port < FIRST_APP_PORT + 1000 => port += 1,
            Err(e) => return Err(format!("no free app port: {e}")),
        }
    };
    tokio::spawn(async move {
        let make_service =
            router.into_make_service_with_connect_info::<std::net::SocketAddr>();
        if let Err(e) = axum::serve(listener, make_service).await {
            tracing::warn!("app server stopped: {e}");
        }
    });
    state.served.lock().unwrap().insert(app.name.clone(), port);
    tracing::info!(app = %app.name, port, "serving app");
    Ok(port)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_github_repo_root() {
        let target = parse_grow_url("grow://github.com/owner/repo").unwrap();
        assert_eq!(
            target,
            GrowTarget {
                host: "github.com".into(),
                owner: "owner".into(),
                repo: "repo".into(),
                path: "/".into(),
            }
        );
        assert_eq!(target.clone_source(), "https://github.com/owner/repo.git");
    }

    #[test]
    fn parses_gitlab_repo_with_inner_path() {
        let target = parse_grow_url("grow://gitlab.com/group/app/docs/guide/").unwrap();
        assert_eq!(target.host, "gitlab.com");
        assert_eq!(target.path, "/docs/guide");
    }

    #[test]
    fn strips_git_suffix_and_www_prefix() {
        let target = parse_grow_url("grow://www.github.com/owner/repo.git").unwrap();
        assert_eq!(target.host, "github.com");
        assert_eq!(target.repo, "repo");
    }

    #[test]
    fn rejects_unknown_host() {
        let err = parse_grow_url("grow://example.com/owner/repo").unwrap_err();
        assert!(err.contains("not supported"), "got: {err}");
    }

    #[test]
    fn rejects_missing_segments() {
        assert!(parse_grow_url("grow://github.com/owner").is_err());
        assert!(parse_grow_url("grow://github.com/").is_err());
        assert!(parse_grow_url("grow://").is_err());
    }

    #[test]
    fn rejects_path_traversal() {
        let err = parse_grow_url("grow://github.com/owner/repo/../../etc").unwrap_err();
        assert!(err.contains("unsafe path"), "got: {err}");
    }

    #[test]
    fn rejects_non_grow_scheme() {
        assert!(parse_grow_url("https://github.com/owner/repo").is_err());
    }
}
