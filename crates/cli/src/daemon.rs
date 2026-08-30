//! `grow daemon` — the `grow://` resolver control plane for the browser
//! extension.
//!
//! The extension cannot speak to git or spawn processes, so it asks this
//! daemon over plain localhost HTTP. `GET /resolve?u=grow://host/owner/repo[/path]`
//! installs the app on first use (https clone from github.com or gitlab.com,
//! gated by [`crate::install_into`]), serves it on a per-app localhost port
//! with the same approval/storage policy as `grow serve`, and answers with
//! the URL the browser should navigate to.

use std::path::{Path, PathBuf};

use axum::extract::{Query, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Deserialize;
use tokio::net::TcpListener;

use grow_server::resolve::{Resolver, parse_grow_url};

use crate::{InstalledApp, enforce_install_policy, install_into, installed_apps_in, resolve_in};

/// Default localhost port for the control plane.
pub const DEFAULT_PORT: u16 = 7890;

type BoxError = Box<dyn std::error::Error>;

#[derive(Clone)]
struct DaemonState {
    root: PathBuf,
    resolver: Resolver,
}

/// Run the control plane on `127.0.0.1:port` until interrupted.
pub fn run(root: &Path, port: u16) -> Result<(), BoxError> {
    let runtime = tokio::runtime::Runtime::new()?;
    runtime.block_on(async move {
        let state = DaemonState {
            root: root.to_path_buf(),
            resolver: Resolver::new(),
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
    for (app, port) in state.resolver.apps() {
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
    let apps: Vec<serde_json::Value> = state
        .resolver
        .apps()
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

/// Resolve a grow:// URL to a running app server, installing and starting
/// the app when needed.
async fn resolve_grow_url(state: &DaemonState, url: &str) -> Result<serde_json::Value, String> {
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
    if let Some(port) = state.resolver.port_for(&app.name) {
        return Ok(port);
    }

    let resolved = resolve_in(&state.root, &app.name).map_err(|e| e.to_string())?;
    let repo = grow_git::Repository::open(&resolved.repo_path).map_err(|e| e.to_string())?;
    let storage =
        enforce_install_policy(&state.root, &resolved, &repo).map_err(|e| e.to_string())?;
    let app_state =
        grow_server::load_with_storage(&resolved.repo_path, storage).map_err(|e| e.to_string())?;
    state.resolver.serve_app(&app.name, app_state).await
}
