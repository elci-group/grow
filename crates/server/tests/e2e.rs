//! End-to-end test: real git repository → grow-server → HTTP requests.

use std::fs;
use std::process::Command;

use axum::body::{Body, to_bytes};
use axum::http::{Request, StatusCode};
use tempfile::TempDir;
use tower::ServiceExt;

const MANIFEST: &str = r#"
[grow]
name = "e2e-app"
version = "0.1.0"

[runtime]
engine = "wasm"
entry = "main.wasm"

[permissions]
filesystem = "restricted"
"#;

/// Same echo module shape as the grow-runtime unit tests: fixed response
/// head followed by the request frame read from stdin.
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

fn fixture_repo() -> TempDir {
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
    fs::create_dir_all(dir.path().join(".grow")).unwrap();
    fs::write(dir.path().join(".grow/manifest.toml"), MANIFEST).unwrap();
    fs::write(dir.path().join("index.html"), "<h1>e2e static index</h1>\n").unwrap();
    let wasm = wat::parse_str(ECHO_WAT).expect("WAT compiles");
    fs::write(dir.path().join("main.wasm"), wasm).unwrap();
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

async fn request(
    app: axum::Router,
    method: &str,
    path: &str,
    body: Body,
) -> (StatusCode, String, String) {
    let response = app
        .oneshot(
            Request::builder()
                .method(method)
                .uri(path)
                .extension(axum::extract::ConnectInfo(std::net::SocketAddr::from((
                    [127, 0, 0, 1],
                    12345,
                ))))
                .body(body)
                .expect("request builds"),
        )
        .await
        .expect("router responds");
    let status = response.status();
    let head = format!("{:?}", response.headers());
    let body = String::from_utf8_lossy(
        &to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("response body reads"),
    )
    .into_owned();
    (status, head, body)
}

#[tokio::test]
async fn serves_static_wasm_and_blocks_traversal() {
    let repo = fixture_repo();
    let state = grow_server::load(repo.path()).expect("repository loads");
    fs::write(
        repo.path().join("index.html"),
        "uncommitted index replacement\n",
    )
    .unwrap();
    let app = grow_server::app(state);

    // `/` maps to index.html, served statically.
    let (status, head, body) = request(app.clone(), "GET", "/", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(head.to_lowercase().contains("content-type"), "{head}");
    assert!(body.contains("e2e static index"), "{body}");
    assert!(!body.contains("uncommitted index replacement"), "{body}");

    // Non-file paths are handled by the WASM entry module.
    let (status, _head, body) = request(app.clone(), "GET", "/api/thing", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body.starts_with("grow wasm says: GET /api/thing\n"),
        "{body}"
    );

    // Traversal never reaches the filesystem (and must not leak files).
    let (_status, _head, body) = request(app.clone(), "GET", "/../Cargo.toml", Body::empty()).await;
    assert!(!body.contains("[workspace]"), "{body}");

    // Liveness probe.
    let (status, _head, body) = request(app.clone(), "GET", "/healthz", Body::empty()).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "ok\n");

    let oversized = vec![b'x'; grow_server::MAX_REQUEST_BODY_BYTES + 1];
    let (status, _head, _body) = request(app, "POST", "/api/thing", Body::from(oversized)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}
