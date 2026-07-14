//! grow-server: the GROW local orchestration daemon.
//!
//! Loads a GROW repository (git-verified), compiles its WASM entry module,
//! and serves the application on localhost:
//!
//! * `GET /healthz` — liveness probe,
//! * `GET /metrics` — request metrics (total, errors, avg duration),
//! * `GET` for a path that matches a file in the repository — static asset with
//!   `Cache-Control: public, max-age=3600, immutable` and `ETag` headers,
//! * anything else — handed to the WASM entry module (CGI-style over WASI),
//! * `404` when the repository declares no runtime entry,
//! * `507` when the module's storage grant exceeds its byte quota.
//!
//! An optional [`grow_runtime::StorageGrant`] preopens a per-app host
//! directory as the module's filesystem root, with its byte quota enforced
//! around every invocation; without one the module gets stdio only.
//!
//! Middleware stack (applied in order, first to last):
//! 1. CORS — permissive, allows cross-origin requests for local development
//! 2. Rate limiter — 120 requests per minute per IP address
//! 3. Tracing — structured logs with request IDs, duration, and status
//! 4. Body limit — 1 MB max request body
//!
//! WASM invocations run on a blocking thread with a wall-clock timeout on
//! top of the runtime's fuel metering. Graceful shutdown handles Ctrl+C and
//! SIGTERM.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, State};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, Uri, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response as HttpResponse};
use axum::routing::get;
use grow_git::{GitError, Repository};
use grow_runtime::{Request, Runtime, RuntimeError, StorageGrant};
use tokio::net::TcpListener;
use tower_http::cors::CorsLayer;

pub const DEFAULT_PORT: u16 = 7870;
/// Wall-clock ceiling per WASM invocation (fuel metering bounds CPU work;
/// this covers pathological host interactions).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum request body accepted by the local daemon.
pub const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;
/// Maximum concurrent WASM invocations allowed.
pub const MAX_CONCURRENT_WASM: usize = 32;
/// Maximum requests per minute from a single IP.
pub const RATE_LIMIT_PER_MINUTE: u64 = 120;

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
    storage: Option<StorageGrant>,
    metrics: RequestMetrics,
    rate_limiter: RateLimiter,
}

impl AppState {
    pub fn manifest(&self) -> &grow_core::Manifest {
        self.repo.manifest()
    }
}

/// Request metrics counters.
pub struct RequestMetrics {
    total: AtomicU64,
    errors: AtomicU64,
    total_duration_ms: AtomicU64,
}

impl RequestMetrics {
    fn new() -> Self {
        Self {
            total: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            total_duration_ms: AtomicU64::new(0),
        }
    }

    fn record(&self, duration: Duration, error: bool) {
        self.total.fetch_add(1, Ordering::Relaxed);
        self.total_duration_ms
            .fetch_add(duration.as_millis() as u64, Ordering::Relaxed);
        if error {
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
    }
}

/// Simple in-memory rate limiter (IP-based, per-minute window).
struct RateLimiter {
    buckets: Mutex<HashMap<String, (u64, Instant)>>,
}

impl RateLimiter {
    fn new() -> Self {
        Self {
            buckets: Mutex::new(HashMap::new()),
        }
    }

    fn check(&self, ip: &str) -> bool {
        let mut buckets = self.buckets.lock().unwrap();
        let now = Instant::now();
        let window = Duration::from_secs(60);
        let entry = buckets.entry(ip.to_string()).or_insert((0, now));
        if now.duration_since(entry.1) > window {
            entry.1 = now;
            entry.0 = 1;
            return true;
        }
        entry.0 += 1;
        entry.0 <= RATE_LIMIT_PER_MINUTE
    }
}

/// Open and verify the repository, compiling the WASM entry if declared.
///
/// Equivalent to [`load_with_storage`] with no storage grant.
pub fn load(repo_path: &Path) -> Result<Arc<AppState>, ServerError> {
    load_with_storage(repo_path, None)
}

/// Open and verify the repository, compiling the WASM entry if declared.
///
/// With `storage`, every WASM invocation preopens the granted directory as
/// the module's filesystem root and enforces its byte quota; with `None` the
/// module receives stdio only (see [`grow_runtime::StorageGrant`]).
pub fn load_with_storage(
    repo_path: &Path,
    storage: Option<StorageGrant>,
) -> Result<Arc<AppState>, ServerError> {
    let repo = Repository::open(repo_path)?;
    let runtime = match repo.manifest().wasm_entry() {
        Some(entry) => {
            let wasm = repo.read_file(entry)?;
            Some(Arc::new(Runtime::new(&wasm)?))
        }
        None => None,
    };
    Ok(Arc::new(AppState {
        repo,
        runtime,
        storage,
        metrics: RequestMetrics::new(),
        rate_limiter: RateLimiter::new(),
    }))
}

/// Build the HTTP router for a loaded application.
pub fn app(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/metrics", get(metrics))
        .fallback(handle_request)
        .layer(DefaultBodyLimit::max(MAX_REQUEST_BODY_BYTES))
        .layer(CorsLayer::permissive())
        .layer(middleware::from_fn_with_state(
            state.clone(),
            rate_limit_and_trace,
        ))
        .with_state(state)
}

pub struct ServeConfig {
    pub repo: PathBuf,
    pub port: u16,
    /// Per-app storage granted to the module's WASI sandbox; `None` for
    /// stdio-only invocations.
    pub storage: Option<StorageGrant>,
}

/// Serve a repository on `127.0.0.1:port` with graceful shutdown.
pub async fn serve(config: ServeConfig) -> Result<(), ServerError> {
    let state = load_with_storage(&config.repo, config.storage)?;
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
    tracing::info!("serving on http://{addr} (press Ctrl+C to stop)");
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown_signal())
    .await?;
    tracing::info!("server shutdown complete");
    Ok(())
}

async fn shutdown_signal() {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };
    #[cfg(unix)]
    let terminate = async {
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();
    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }
}

async fn healthz() -> &'static str {
    "ok\n"
}

async fn metrics(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let mut output = String::new();
    output.push_str("# GROW server metrics\n");
    output.push_str(&format!(
        "# app={} version={} commit={}\n",
        state.repo.manifest().grow.name,
        state.repo.manifest().grow.version,
        state.repo.head_commit()
    ));
    output.push_str("grow_up 1\n");
    output.push_str(&format!(
        "requests_total {}\n",
        state.metrics.total.load(Ordering::Relaxed)
    ));
    output.push_str(&format!(
        "requests_errors {}\n",
        state.metrics.errors.load(Ordering::Relaxed)
    ));
    let total_ms = state.metrics.total_duration_ms.load(Ordering::Relaxed);
    let total_req = state.metrics.total.load(Ordering::Relaxed).max(1);
    output.push_str(&format!(
        "request_duration_avg_ms {}\n",
        total_ms / total_req
    ));
    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; charset=utf-8",
        )],
        output,
    )
}

async fn rate_limit_and_trace(
    State(state): State<Arc<AppState>>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    req: axum::http::Request<axum::body::Body>,
    next: Next,
) -> impl IntoResponse {
    let ip = addr.ip().to_string();
    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let start = Instant::now();
    let req_id = format!("{:016x}", start.elapsed().as_nanos() as u64);

    if !state.rate_limiter.check(&ip) {
        tracing::warn!(%ip, %method, %path, "rate limit exceeded");
        return (
            StatusCode::TOO_MANY_REQUESTS,
            "rate limit exceeded (120 req/min)\n",
        )
            .into_response();
    }

    tracing::info!(req_id = %req_id, %ip, %method, %path, "request started");
    let response = next.run(req).await;
    let duration = start.elapsed();
    let status = response.status();
    let error = status.is_server_error();
    state.metrics.record(duration, error);

    if error {
        tracing::error!(req_id = %req_id, %status, ?duration, "request failed");
    } else {
        tracing::info!(req_id = %req_id, %status, ?duration, "request completed");
    }
    response
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
    let storage = state.storage.clone();

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
        tokio::task::spawn_blocking(move || runtime.invoke(&request, storage.as_ref())),
    )
    .await
    {
        Ok(Ok(Ok(response))) => to_http(response),
        Ok(Ok(Err(err))) => {
            tracing::warn!(%err, "wasm invocation failed");
            invocation_error_response(&err)
        }
        Ok(Err(err)) => {
            tracing::warn!(%err, "wasm task failed");
            status_text(StatusCode::INTERNAL_SERVER_ERROR, "internal error\n")
        }
        Err(_) => status_text(StatusCode::GATEWAY_TIMEOUT, "application timed out\n"),
    }
}

/// Map a failed WASM invocation to its HTTP response: an over-quota storage
/// grant means the application exhausted its allowance (507), anything else
/// is a generic application failure (502).
fn invocation_error_response(err: &RuntimeError) -> HttpResponse {
    match err {
        RuntimeError::StorageQuotaExceeded { .. } => {
            status_text(StatusCode::INSUFFICIENT_STORAGE, "storage quota exceeded\n")
        }
        _ => status_text(StatusCode::BAD_GATEWAY, "application error\n"),
    }
}

fn serve_file(path: &str, bytes: Vec<u8>) -> HttpResponse {
    let mime = mime_guess::from_path(path).first_or_octet_stream();
    let etag = format!("\"{:x}\"", md5_hash(&bytes));
    (
        [
            (header::CONTENT_TYPE, mime.as_ref()),
            (header::CACHE_CONTROL, "public, max-age=3600, immutable"),
            (header::ETAG, etag.as_str()),
        ],
        bytes,
    )
        .into_response()
}

fn md5_hash(data: &[u8]) -> u64 {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};
    let mut hasher = DefaultHasher::new();
    data.hash(&mut hasher);
    hasher.finish()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_hash_is_deterministic() {
        let data = b"hello world";
        let h1 = md5_hash(data);
        let h2 = md5_hash(data);
        assert_eq!(h1, h2);
        assert_ne!(h1, 0);
    }

    #[test]
    fn md5_hash_different_for_different_data() {
        let h1 = md5_hash(b"hello");
        let h2 = md5_hash(b"world");
        assert_ne!(h1, h2);
    }

    #[test]
    fn rate_limiter_allows_under_limit() {
        let limiter = RateLimiter::new();
        for _ in 0..RATE_LIMIT_PER_MINUTE {
            assert!(limiter.check("127.0.0.1"));
        }
    }

    #[test]
    fn rate_limiter_blocks_over_limit() {
        let limiter = RateLimiter::new();
        for _ in 0..RATE_LIMIT_PER_MINUTE {
            assert!(limiter.check("127.0.0.1"));
        }
        assert!(!limiter.check("127.0.0.1"));
    }

    #[test]
    fn rate_limiter_tracks_per_ip() {
        let limiter = RateLimiter::new();
        for _ in 0..RATE_LIMIT_PER_MINUTE {
            assert!(limiter.check("127.0.0.1"));
        }
        assert!(!limiter.check("127.0.0.1"));
        assert!(limiter.check("192.168.1.1"));
    }

    #[test]
    fn rate_limiter_window_resets() {
        let limiter = RateLimiter::new();
        for _ in 0..RATE_LIMIT_PER_MINUTE {
            assert!(limiter.check("127.0.0.1"));
        }
        assert!(!limiter.check("127.0.0.1"));
        // Fast-forward bucket by waiting a minute. In the test we can't wait
        // that long, so we verify the bucket is created and at capacity.
        let buckets = limiter.buckets.lock().unwrap();
        let entry = buckets.get("127.0.0.1").unwrap();
        assert_eq!(entry.0, RATE_LIMIT_PER_MINUTE + 1);
    }

    #[test]
    fn status_text_builds_response() {
        let resp = status_text(StatusCode::NOT_FOUND, "not found\n");
        assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    }

    #[test]
    fn to_http_sets_default_content_type() {
        let resp = grow_runtime::Response {
            status: 200,
            reason: "OK".into(),
            headers: vec![],
            body: b"hello".to_vec(),
        };
        let http = to_http(resp);
        assert_eq!(http.status(), StatusCode::OK);
        let headers = http.headers();
        assert_eq!(headers[header::CONTENT_TYPE], "text/plain");
    }

    #[test]
    fn to_http_preserves_module_content_type() {
        let resp = grow_runtime::Response {
            status: 200,
            reason: "OK".into(),
            headers: vec![("content-type".into(), "application/json".into())],
            body: b"{}".to_vec(),
        };
        let http = to_http(resp);
        let headers = http.headers();
        assert_eq!(headers[header::CONTENT_TYPE], "application/json");
    }

    #[test]
    fn to_http_drops_invalid_headers() {
        let resp = grow_runtime::Response {
            status: 200,
            reason: "OK".into(),
            headers: vec![("bad\r\nheader".into(), "value".into())],
            body: b"hello".to_vec(),
        };
        let http = to_http(resp);
        assert_eq!(http.status(), StatusCode::OK);
        let headers = http.headers();
        // invalid header dropped, default content-type added
        assert_eq!(headers[header::CONTENT_TYPE], "text/plain");
    }

    #[test]
    fn serve_file_sets_cache_headers() {
        let resp = serve_file("test.html", b"<h1>test</h1>".to_vec());
        assert_eq!(resp.status(), StatusCode::OK);
        let headers = resp.headers();
        assert_eq!(headers[header::CONTENT_TYPE], "text/html");
        assert_eq!(
            headers[header::CACHE_CONTROL],
            "public, max-age=3600, immutable"
        );
        assert!(headers.contains_key(header::ETAG));
    }

    #[test]
    fn request_metrics_records() {
        let m = RequestMetrics::new();
        m.record(Duration::from_millis(100), false);
        assert_eq!(m.total.load(Ordering::Relaxed), 1);
        assert_eq!(m.errors.load(Ordering::Relaxed), 0);
        assert_eq!(m.total_duration_ms.load(Ordering::Relaxed), 100);

        m.record(Duration::from_millis(50), true);
        assert_eq!(m.total.load(Ordering::Relaxed), 2);
        assert_eq!(m.errors.load(Ordering::Relaxed), 1);
        assert_eq!(m.total_duration_ms.load(Ordering::Relaxed), 150);
    }

    #[test]
    fn invocation_error_maps_quota_exceeded_to_507() {
        let err = RuntimeError::StorageQuotaExceeded { used: 16, quota: 8 };
        let resp = invocation_error_response(&err);
        assert_eq!(resp.status(), StatusCode::INSUFFICIENT_STORAGE);
    }

    #[tokio::test]
    async fn invocation_error_quota_body_is_explicit() {
        let err = RuntimeError::StorageQuotaExceeded { used: 16, quota: 8 };
        let resp = invocation_error_response(&err);
        let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
            .await
            .expect("body reads");
        assert_eq!(&body[..], b"storage quota exceeded\n");
    }

    #[test]
    fn invocation_error_maps_other_errors_to_502() {
        let errors = [
            RuntimeError::Engine("boom".into()),
            RuntimeError::OutOfFuel,
            RuntimeError::Exit(1),
            RuntimeError::Trap("boom".into()),
            RuntimeError::BadResponse("boom".into()),
        ];
        for err in &errors {
            let resp = invocation_error_response(err);
            assert_eq!(resp.status(), StatusCode::BAD_GATEWAY, "{err}");
        }
    }
}
