//! `grow://` URL grammar and per-app serving registry.
//!
//! Shared by the desktop daemon (`grow daemon`, driving the browser
//! extension) and the on-device browser engine (`grow-mobile`): both parse
//! `grow://host/owner/repo[/path...]` URLs the same way and serve each
//! installed app on its own localhost port. Keeping the grammar and the
//! port registry here means the two front-ends cannot drift apart.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use tokio::net::TcpListener;

use crate::AppState;

/// Apps are served on sequential ports starting here, skipping any port
/// already taken by another process.
const FIRST_APP_PORT: u16 = 7901;

/// A parsed `grow://host/owner/repo[/path...]` URL.
#[derive(Debug, Clone, PartialEq)]
pub struct GrowTarget {
    pub host: String,
    pub owner: String,
    pub repo: String,
    /// Path within the served site, always rooted (`/` when absent).
    pub path: String,
}

impl GrowTarget {
    /// The https clone source for this repository.
    pub fn clone_source(&self) -> String {
        format!("https://{}/{}/{}.git", self.host, self.owner, self.repo)
    }
}

/// Parse a `grow://` URL. The first two path segments are the owner and
/// repository (gitlab subgroups are not supported yet); any remaining
/// segments become the path navigated to within the served site.
pub fn parse_grow_url(url: &str) -> Result<GrowTarget, String> {
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

/// Per-app localhost server registry. Cheap to clone; all clones share the
/// same bookkeeping.
#[derive(Clone, Default)]
pub struct Resolver {
    served: Arc<Mutex<HashMap<String, u16>>>,
}

impl Resolver {
    pub fn new() -> Self {
        Self::default()
    }

    /// The port `app` is already served on, if any.
    pub fn port_for(&self, app: &str) -> Option<u16> {
        self.served.lock().unwrap().get(app).copied()
    }

    /// Snapshot of `(app, port)` pairs currently served, sorted by app name.
    pub fn apps(&self) -> Vec<(String, u16)> {
        let served = self.served.lock().unwrap().clone();
        let mut apps: Vec<(String, u16)> = served.into_iter().collect();
        apps.sort();
        apps
    }

    /// Serve `state` for `app` on its own localhost port, starting a server
    /// task on first use; subsequent calls return the existing port.
    pub async fn serve_app(&self, app: &str, state: Arc<AppState>) -> Result<u16, String> {
        if let Some(port) = self.port_for(app) {
            return Ok(port);
        }
        let router = crate::app(state);

        let mut port = self
            .served
            .lock()
            .unwrap()
            .values()
            .max()
            .map_or(FIRST_APP_PORT, |max| max + 1);
        let listener = loop {
            match TcpListener::bind(std::net::SocketAddr::from(([127, 0, 0, 1], port))).await {
                Ok(listener) => break listener,
                Err(_) if port < FIRST_APP_PORT + 1000 => port += 1,
                Err(e) => return Err(format!("no free app port: {e}")),
            }
        };
        tokio::spawn(async move {
            let make_service = router.into_make_service_with_connect_info::<std::net::SocketAddr>();
            if let Err(e) = axum::serve(listener, make_service).await {
                tracing::warn!("app server stopped: {e}");
            }
        });
        self.served.lock().unwrap().insert(app.to_string(), port);
        tracing::info!(app, port, "serving app");
        Ok(port)
    }
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
