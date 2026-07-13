//! grow-server: the GROW local orchestration daemon.
//!
//! Loads a GROW repository (git-verified), compiles its WASM entry module,
//! and serves the application on localhost:
//!
//! * `GET /healthz` — liveness probe,
//! * `GET` for a path that matches a file in the repository — static asset,
//! * anything else — handed to the WASM entry module (CGI-style over WASI),
//! * `404` when the repository declares no runtime entry.
//!
//! WASM invocations run on a blocking thread with a wall-clock timeout on
//! top of the runtime's fuel metering.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header};
use axum::response::{IntoResponse, Response as HttpResponse};
use axum::routing::get;
use grow_git::{GitError, Repository};
use grow_runtime::{Request, Runtime};
use tokio::net::TcpListener;

pub const DEFAULT_PORT: u16 = 7870;
/// Wall-clock ceiling per WASM invocation (fuel metering bounds CPU work;
/// this covers pathological host interactions).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum request body accepted by the local daemon.
pub const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ServerError {
    #[error(transparent)]
    Git(#[from] grow_git::GitError),
    #[error(transparent)]
    Runtime(#[from] grow_runtime::RuntimeError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

/// Shared daemon state: the loaded repository and its compiled entry module.
pub struct AppState {
    repo: Repository,
    runtime: Option<Arc<Runtime>>,
}

impl AppState {
    pub fn manifest(&self) -> &grow_core::Manifest {
        self.repo.manifest()
    }
}

/// Open and verify the repository, compiling the WASM entry if declared.
pub fn load(repo_path: &Path) -> Result<Arc<AppState>, ServerError> {
    let repo = Repository::open(repo_path)?;
    let runtime = match repo.manifest().wasm_entry() {
        Some(entry) => {
            let wasm = repo.read_file(entry)?;
            Some(Arc::new(Runtime::new(&wasm)?))
        }
        None => None,
    };
    Ok(Arc::new(AppState { repo, runtime }))
}

/// Build the HTTP router for a loaded application.
pub fn app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics))
        .fallback(handle_request)
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .with_state(state)
}

pub struct ServeConfig {
    pub repo: PathBuf,
    pub port: u16,
}

/// Serve a repository on `127.0.0.1:port` until the process is stopped.
pub async fn serve(config: ServeConfig) -> Result<(), ServerError> {
    let state = load(&config.repo)?;
    {
        let manifest = state.repo.manifest();
        tracing::info!(
            app = %manifest.grow.name,
            version = %manifest.grow.version,
            commit = %state.repo.head_commit(),
            permissions = ?manifest.permissions,
            network = ?manifest.network.enabled,
            "repository verified"
        );
    }
    let router = app(state);
    let addr = SocketAddr::from(([127, 0, 0, 1], config.port));
    let listener = TcpListener::bind(addr).await?;
    tracing::info!("serving on http://{addr}");
    axum::serve(listener, router).await?;
    Ok(())
}

async fn healthz() -> &'static str {
    "ok\n"
}

async fn metrics(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut output = String::new();
    output.push_str("# GROW server metrics\n");
    output.push_str(&format!("# app={} version={} commit={}\n", 
        state.repo.manifest().grow.name,
        state.repo.manifest().grow.version,
        state.repo.head_commit()));
    output.push_str("grow_up 1\n");
    (StatusCode::OK, [(axum::http::header::CONTENT_TYPE, "text/plain; charset=utf-8")], output)
}

async fn handle_request(
    State(state): State<Arc<AppState>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> HttpResponse {
    if method != Method::GET && method != Method::POST {
        return status_text(StatusCode::METHOD_NOT_ALLOWED, "method not allowed\n");
    }

    // Static assets win for GET: a path that resolves to a real file inside
    // the repository is served directly. `/` maps to `index.html`.
    if method == Method::GET {
        let requested = uri.path().trim_start_matches('/');
        let relative = if requested.is_empty() {
            "index.html"
        } else {
            requested
        };
        match state.repo.read_file(relative) {
            Ok(bytes) => return serve_file(relative, bytes),
            Err(GitError::MissingFile(_) | GitError::NotRegularFile(_)) => {}
            Err(err) => {
                tracing::warn!(%err, path = relative, "failed to read verified asset");
                return status_text(StatusCode::INTERNAL_SERVER_ERROR, "failed to read asset\n");
            }
        }
    }

    let Some(runtime) = state.runtime.clone() else {
        return status_text(StatusCode::NOT_FOUND, "not found\n");
    };

    let request = Request {
        method: method.to_string(),
        path: uri.path().to_string(),
        headers: headers
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|v| (name.as_str().to_string(), v.to_string()))
            })
            .collect(),
        body: body.to_vec(),
    };

    match tokio::time::timeout(
        REQUEST_TIMEOUT,
        tokio::task::spawn_blocking(move || runtime.invoke(&request)),
    )
    .await
    {
        Ok(Ok(Ok(response))) => to_http(response),
        Ok(Ok(Err(err))) => {
            tracing::warn!(%err, "wasm invocation failed");
            status_text(StatusCode::BAD_GATEWAY, "application error\n")
        }
        Ok(Err(err)) => {
            tracing::warn!(%err, "wasm task failed");
            status_text(StatusCode::INTERNAL_SERVER_ERROR, "internal error\n")
        }
        Err(_) => status_text(StatusCode::GATEWAY_TIMEOUT, "application timed out\n"),
    }
}

fn serve_file(path: &str, bytes: Vec<u8>) -> HttpResponse {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    ([(header::CONTENT_TYPE, mime.as_ref())], bytes).into_response()
}

fn to_http(response: grow_runtime::Response) -> HttpResponse {
    let status = StatusCode::from_u16(response.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut builder = HttpResponse::builder().status(status);
    let mut has_content_type = false;
    for (name, value) in &response.headers {
        if name.eq_ignore_ascii_case("content-type") {
            has_content_type = true;
        }
        match (
            HeaderName::from_bytes(name.as_bytes()),
            HeaderValue::from_str(value),
        ) {
            (Ok(name), Ok(value)) => {
                builder = builder.header(name, value);
            }
            _ => tracing::debug!(%name, "dropping invalid header from module response"),
        }
    }
    if !has_content_type {
        builder = builder.header(header::CONTENT_TYPE, "text/plain");
    }
    builder
        .body(response.body.into())
        .unwrap_or_else(|_| status_text(StatusCode::INTERNAL_SERVER_ERROR, "bad response\n"))
}

fn status_text(status: StatusCode, message: &'static str) -> HttpResponse {
    (status, message).into_response()
}
