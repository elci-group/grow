//! grow-runtime: sandboxed WebAssembly execution for GROW applications.
//!
//! Milestone 1 execution model is CGI-style over WASI preview1:
//!
//! * **Request** is written to the module's stdin:
//!   `METHOD PATH\n`, then `Header: value\n` lines, a blank line, then the body.
//! * **Response** is read from the module's stdout:
//!   `STATUS REASON\n`, then headers, a blank line, then the body.
//!
//! Sandboxing (Milestone 1):
//!
//! * fuel metering bounds execution time deterministically,
//! * linear memory is capped,
//! * the module receives stdio only by default — no filesystem, network, env,
//!   or args; with a [`StorageGrant`], a per-app host directory is preopened as
//!   the module's filesystem root with a byte quota enforced around each
//!   invocation.

use std::path::{Path, PathBuf};
use wasmtime::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, Trap};
use wasmtime_wasi::p1::{self, WasiP1Ctx};
use wasmtime_wasi::p2::pipe::{MemoryInputPipe, MemoryOutputPipe};
use wasmtime_wasi::{DirPerms, FilePerms, I32Exit, WasiCtxBuilder};

/// Stack size limit per module instance (1 MB, must not exceed async stack size).
pub const MAX_STACK_BYTES: usize = 1024 * 1024;

/// Fuel granted per invocation (~1B instructions of headroom).
pub const DEFAULT_FUEL: u64 = 1_000_000_000;
/// Linear-memory cap per module instance.
pub const MAX_MEMORY_BYTES: usize = 256 * 1024 * 1024;
/// Cap on captured stdout/stderr.
pub const MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
/// Maximum request body that a module will receive (1 MB).
pub const MAX_REQUEST_BODY_BYTES: usize = 1024 * 1024;
/// Maximum number of headers in a request.
pub const MAX_HEADERS: usize = 64;
/// Maximum header name length.
pub const MAX_HEADER_NAME_BYTES: usize = 256;
/// Maximum header value length.
pub const MAX_HEADER_VALUE_BYTES: usize = 8192;

/// An application request, framed onto the module's stdin.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Request {
    /// Serialize to the stdin frame format.
    /// Validates limits before framing; returns an error if any limit is exceeded.
    pub fn frame(&self) -> Result<Vec<u8>, RuntimeError> {
        if self.headers.len() > MAX_HEADERS {
            return Err(RuntimeError::BadResponse(format!(
                "too many headers: {} (max {})",
                self.headers.len(),
                MAX_HEADERS
            )));
        }
        for (name, value) in &self.headers {
            if name.len() > MAX_HEADER_NAME_BYTES {
                return Err(RuntimeError::BadResponse(format!(
                    "header name too long: {} bytes (max {})",
                    name.len(),
                    MAX_HEADER_NAME_BYTES
                )));
            }
            if value.len() > MAX_HEADER_VALUE_BYTES {
                return Err(RuntimeError::BadResponse(format!(
                    "header value too long: {} bytes (max {})",
                    value.len(),
                    MAX_HEADER_VALUE_BYTES
                )));
            }
        }
        if self.body.len() > MAX_REQUEST_BODY_BYTES {
            return Err(RuntimeError::BadResponse(format!(
                "body too large: {} bytes (max {})",
                self.body.len(),
                MAX_REQUEST_BODY_BYTES
            )));
        }
        let mut frame = format!("{} {}\n", self.method, self.path).into_bytes();
        for (name, value) in &self.headers {
            frame.extend_from_slice(name.as_bytes());
            frame.extend_from_slice(b": ");
            frame.extend_from_slice(value.as_bytes());
            frame.push(b'\n');
        }
        frame.push(b'\n');
        frame.extend_from_slice(&self.body);
        Ok(frame)
    }
}

/// An application response, parsed from the module's stdout.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub reason: String,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

#[derive(Debug, thiserror::Error)]
pub enum RuntimeError {
    #[error("wasm engine error: {0}")]
    Engine(String),
    #[error("module exceeded its execution budget (out of fuel)")]
    OutOfFuel,
    #[error("module exited with status {0}")]
    Exit(i32),
    #[error("module trapped: {0}")]
    Trap(String),
    #[error("module produced an invalid response: {0}")]
    BadResponse(String),
    #[error("storage quota exceeded: using {used} bytes of {quota} byte limit")]
    StorageQuotaExceeded { used: u64, quota: u64 },
}

/// Parse a module's stdout into a [`Response`].
pub fn parse_response(stdout: &[u8]) -> Result<Response, RuntimeError> {
    let sep = stdout
        .windows(2)
        .position(|w| w == b"\n\n")
        .ok_or_else(|| {
            RuntimeError::BadResponse("missing blank line after response head".into())
        })?;
    let head = std::str::from_utf8(&stdout[..sep])
        .map_err(|_| RuntimeError::BadResponse("response head is not valid UTF-8".into()))?;
    let body = stdout[sep + 2..].to_vec();

    let mut lines = head.lines();
    let status_line = lines
        .next()
        .ok_or_else(|| RuntimeError::BadResponse("empty response head".into()))?;
    let mut parts = status_line.splitn(2, ' ');
    let status: u16 = parts
        .next()
        .and_then(|s| s.parse().ok())
        .filter(|s| (100..=599).contains(s))
        .ok_or_else(|| {
            RuntimeError::BadResponse(format!("invalid status line: {status_line:?}"))
        })?;
    let reason = parts.next().unwrap_or("").to_string();

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| RuntimeError::BadResponse(format!("malformed header line: {line:?}")))?;
        headers.push((name.trim().to_string(), value.trim().to_string()));
    }

    Ok(Response {
        status,
        reason,
        headers,
        body,
    })
}

/// Host directory granted to the module as its filesystem root, with an
/// optional byte quota enforced around each invocation.
#[derive(Debug, Clone)]
pub struct StorageGrant {
    pub dir: PathBuf,
    pub quota_bytes: Option<u64>,
}

impl StorageGrant {
    /// Grant the module `dir` as its filesystem root (`/`), optionally capped
    /// at `quota_bytes` of total file content.
    pub fn new(dir: impl Into<PathBuf>, quota_bytes: Option<u64>) -> Self {
        Self {
            dir: dir.into(),
            quota_bytes,
        }
    }
}

/// Recursive sum of file sizes under `dir`. Symlinks are never followed, so
/// links pointing outside the tree cannot hide or inflate usage. Iterative to
/// keep host stack usage independent of module-created directory depth.
fn dir_size_bytes(dir: &Path) -> std::io::Result<u64> {
    let mut total = 0;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in std::fs::read_dir(dir)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                stack.push(entry.path());
            } else {
                total += entry.metadata()?.len();
            }
        }
    }
    Ok(total)
}

/// Fail with [`RuntimeError::StorageQuotaExceeded`] when the granted directory
/// already holds more than its declared quota.
fn enforce_quota(grant: &StorageGrant) -> Result<(), RuntimeError> {
    if let Some(quota) = grant.quota_bytes {
        let used = dir_size_bytes(&grant.dir).map_err(|e| {
            RuntimeError::Engine(format!(
                "storage error: cannot measure {}: {e}",
                grant.dir.display()
            ))
        })?;
        if used > quota {
            return Err(RuntimeError::StorageQuotaExceeded { used, quota });
        }
    }
    Ok(())
}

/// A compiled GROW application. Cheap to clone-free share: the engine and
/// module are `Send + Sync`; each invocation builds a fresh `Store`.
pub struct Runtime {
    engine: Engine,
    module: Module,
}

struct InvocationCtx {
    wasi: WasiP1Ctx,
    limits: StoreLimits,
}

impl Runtime {
    /// Compile a WASM module for repeated invocation.
    pub fn new(wasm: &[u8]) -> Result<Self, RuntimeError> {
        let mut config = Config::new();
        config.consume_fuel(true);
        // Restrict features to prevent host DoS from malicious modules.
        config.wasm_memory64(false);
        config.wasm_multi_memory(false);
        config.wasm_threads(false);
        let engine = Engine::new(&config).map_err(|e| RuntimeError::Engine(e.to_string()))?;
        let module = Module::new(&engine, wasm).map_err(|e| RuntimeError::Engine(e.to_string()))?;
        Ok(Self { engine, module })
    }

    /// Invoke the module's `_start` with `stdin` as its standard input and
    /// return captured stdout. Synchronous and CPU-bound — callers in async
    /// contexts should use `spawn_blocking` plus a timeout.
    ///
    /// With `storage`, the granted directory is created if missing, preopened
    /// as the module's filesystem root (`/`), and its quota is checked before
    /// and after the run: an over-quota directory fails the request with
    /// [`RuntimeError::StorageQuotaExceeded`] even when the module ran fine.
    /// With `None` the module receives stdio only, exactly as before.
    pub fn run_raw(
        &self,
        stdin: &[u8],
        storage: Option<&StorageGrant>,
    ) -> Result<Vec<u8>, RuntimeError> {
        let stdout = MemoryOutputPipe::new(MAX_OUTPUT_BYTES);
        let stderr = MemoryOutputPipe::new(MAX_OUTPUT_BYTES);

        let mut builder = WasiCtxBuilder::new();
        builder
            .stdin(MemoryInputPipe::new(stdin.to_vec()))
            .stdout(stdout.clone())
            .stderr(stderr.clone());
        // Deliberately no env or args. Without a storage grant, no preopened
        // dirs either: stdio only.
        if let Some(grant) = storage {
            std::fs::create_dir_all(&grant.dir).map_err(|e| {
                RuntimeError::Engine(format!(
                    "storage error: cannot create {}: {e}",
                    grant.dir.display()
                ))
            })?;
            enforce_quota(grant)?;
            builder
                .preopened_dir(&grant.dir, "/", DirPerms::all(), FilePerms::all())
                .map_err(|e| {
                    RuntimeError::Engine(format!(
                        "storage error: cannot preopen {}: {e}",
                        grant.dir.display()
                    ))
                })?;
        }
        let wasi = builder.build_p1();

        let mut store = Store::new(
            &self.engine,
            InvocationCtx {
                wasi,
                limits: StoreLimitsBuilder::new()
                    .memory_size(MAX_MEMORY_BYTES)
                    .build(),
            },
        );
        store.limiter(|ctx| &mut ctx.limits);
        store
            .set_fuel(DEFAULT_FUEL)
            .map_err(|e| RuntimeError::Engine(e.to_string()))?;

        let mut linker = Linker::<InvocationCtx>::new(&self.engine);
        p1::add_to_linker_sync(&mut linker, |ctx| &mut ctx.wasi)
            .map_err(|e| RuntimeError::Engine(e.to_string()))?;

        let result = (|| {
            let instance = linker.instantiate(&mut store, &self.module)?;
            let start = instance.get_typed_func::<(), ()>(&mut store, "_start")?;
            start.call(&mut store, ())
        })();

        match result {
            Ok(()) => {
                if let Some(grant) = storage {
                    enforce_quota(grant)?;
                }
                Ok(stdout.contents().to_vec())
            }
            Err(err) => {
                if let Some(exit) = err.downcast_ref::<I32Exit>() {
                    if exit.0 == 0 {
                        if let Some(grant) = storage {
                            enforce_quota(grant)?;
                        }
                        return Ok(stdout.contents().to_vec());
                    }
                    return Err(RuntimeError::Exit(exit.0));
                }
                if matches!(err.downcast_ref::<Trap>(), Some(Trap::OutOfFuel)) {
                    return Err(RuntimeError::OutOfFuel);
                }
                Err(RuntimeError::Trap(err.to_string()))
            }
        }
    }

    /// Invoke the module and parse its stdout as a [`Response`]. The `storage`
    /// grant behaves exactly as in [`Runtime::run_raw`].
    pub fn invoke(
        &self,
        request: &Request,
        storage: Option<&StorageGrant>,
    ) -> Result<Response, RuntimeError> {
        parse_response(&self.run_raw(&request.frame()?, storage)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// WASI module that echoes the request back after a fixed response head.
    /// Memory layout: response head at 0 (49 bytes), request buffer at
    /// [256, 768), iovecs and scratch at 1024+.
    pub const ECHO_WAT: &str = r#"(module
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

    fn echo_runtime() -> Runtime {
        let wasm = wat::parse_str(ECHO_WAT).expect("WAT compiles");
        Runtime::new(&wasm).expect("runtime builds")
    }

    #[test]
    fn echoes_request_line_in_body() {
        let runtime = echo_runtime();
        let response = runtime
            .invoke(
                &Request {
                    method: "GET".into(),
                    path: "/hello".into(),
                    headers: vec![("Host".into(), "localhost".into())],
                    body: Vec::new(),
                },
                None,
            )
            .expect("invocation succeeds");
        assert_eq!(response.status, 200);
        assert_eq!(response.reason, "OK");
        assert!(
            response
                .headers
                .iter()
                .any(|(k, v)| k == "Content-Type" && v == "text/plain")
        );
        let body = String::from_utf8(response.body).unwrap();
        assert!(
            body.starts_with("grow wasm says: GET /hello\n"),
            "unexpected body: {body:?}"
        );
        assert!(body.contains("Host: localhost"));
    }

    #[test]
    fn infinite_loop_runs_out_of_fuel() {
        let wasm =
            wat::parse_str(r#"(module (func (export "_start") (loop $l (br $l))))"#).unwrap();
        let runtime = Runtime::new(&wasm).unwrap();
        let err = runtime.run_raw(&[], None).unwrap_err();
        assert!(matches!(err, RuntimeError::OutOfFuel), "got: {err}");
    }

    #[test]
    fn nonzero_exit_is_reported() {
        let wasm = wat::parse_str(
            r#"(module
              (import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (param i32)))
              (memory (export "memory") 1)
              (func (export "_start") (call $proc_exit (i32.const 3))))"#,
        )
        .unwrap();
        let runtime = Runtime::new(&wasm).unwrap();
        let err = runtime.run_raw(&[], None).unwrap_err();
        assert!(matches!(err, RuntimeError::Exit(3)), "got: {err}");
    }

    #[test]
    fn malformed_output_is_rejected() {
        assert!(parse_response(b"no blank line here").is_err());
        assert!(parse_response(b"999 Weird\n\nbody").is_err());
        assert!(parse_response(b"200 OK\nBad-Header-Line\n\nbody").is_err());
        let ok = parse_response(b"204 No Content\n\n").unwrap();
        assert_eq!(ok.status, 204);
        assert!(ok.body.is_empty());
    }

    /// WASI module that writes 16 bytes to `storage-test.txt` under the
    /// preopened root (fd 3): `path_open` with OFLAG_CREAT, `fd_write`,
    /// `fd_close`. `path_open` args: dir fd 3, lookupflags 0, path ptr/len,
    /// oflags 1 (CREAT), rights 66 (FD_READ | FD_WRITE), fdflags 0. A nonzero
    /// WASI errno aborts `_start` with it as the exit code.
    ///
    /// Memory layout: file body at [0, 16), path string at [64, 80), iovec at
    /// 1024, opened-fd result at 1032, nwritten scratch at 1036.
    pub const WRITE_WAT: &str = r#"(module
  (import "wasi_snapshot_preview1" "path_open" (func $path_open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_close" (func $fd_close (param i32) (result i32)))
  (import "wasi_snapshot_preview1" "proc_exit" (func $proc_exit (param i32)))
  (memory (export "memory") 1)
  (data (i32.const 0) "0123456789abcdef")
  (data (i32.const 64) "storage-test.txt")
  (func (export "_start")
    (local $errno i32)
    (local.set $errno
      (call $path_open
        (i32.const 3)
        (i32.const 0)
        (i32.const 64)
        (i32.const 16)
        (i32.const 1)
        (i64.const 66)
        (i64.const 0)
        (i32.const 0)
        (i32.const 1032)))
    (if (local.get $errno) (then (call $proc_exit (local.get $errno))))
    (i32.store (i32.const 1024) (i32.const 0))
    (i32.store (i32.const 1028) (i32.const 16))
    (local.set $errno
      (call $fd_write (i32.load (i32.const 1032)) (i32.const 1024) (i32.const 1) (i32.const 1036)))
    (if (local.get $errno) (then (call $proc_exit (local.get $errno))))
    (local.set $errno (call $fd_close (i32.load (i32.const 1032))))
    (if (local.get $errno) (then (call $proc_exit (local.get $errno))))
    (call $proc_exit (i32.const 0))
  )
)"#;

    fn write_runtime() -> Runtime {
        let wasm = wat::parse_str(WRITE_WAT).expect("WAT compiles");
        Runtime::new(&wasm).expect("runtime builds")
    }

    /// Fresh, empty host directory for a storage test, unique per test name
    /// and process so reruns start clean.
    fn temp_storage_dir(test: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("grow-runtime-{test}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir created");
        dir
    }

    #[test]
    fn write_under_quota_persists_on_host() {
        let dir = temp_storage_dir("write-under-quota");
        let runtime = write_runtime();
        let grant = StorageGrant::new(&dir, Some(256));
        runtime
            .run_raw(&[], Some(&grant))
            .expect("invocation succeeds");
        let written = std::fs::read(dir.join("storage-test.txt")).expect("file persists on host");
        assert_eq!(written, b"0123456789abcdef");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn write_over_quota_fails_after_run() {
        let dir = temp_storage_dir("write-over-quota");
        let runtime = write_runtime();
        let grant = StorageGrant::new(&dir, Some(8));
        let err = runtime.run_raw(&[], Some(&grant)).unwrap_err();
        assert!(
            matches!(
                err,
                RuntimeError::StorageQuotaExceeded { used: 16, quota: 8 }
            ),
            "got: {err}"
        );
        // The module did run; the quota check fails the request afterwards.
        assert!(dir.join("storage-test.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dir_already_over_quota_fails_before_run() {
        let dir = temp_storage_dir("preexisting-over-quota");
        std::fs::write(dir.join("pre-existing.bin"), [0u8; 32]).expect("seed file");
        let runtime = write_runtime();
        let grant = StorageGrant::new(&dir, Some(16));
        let err = runtime.run_raw(&[], Some(&grant)).unwrap_err();
        assert!(
            matches!(
                err,
                RuntimeError::StorageQuotaExceeded {
                    used: 32,
                    quota: 16
                }
            ),
            "got: {err}"
        );
        // The module must not have run: no new file in the granted dir.
        assert!(!dir.join("storage-test.txt").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_storage_grant_means_no_filesystem() {
        let runtime = write_runtime();
        let err = runtime.run_raw(&[], None).unwrap_err();
        // Without a grant fd 3 is not a preopen: path_open fails and the
        // module exits nonzero with the WASI errno.
        assert!(
            matches!(err, RuntimeError::Exit(code) if code != 0),
            "got: {err}"
        );
    }

    #[test]
    fn dir_size_counts_nested_files() {
        let dir = temp_storage_dir("dir-size-nested");
        std::fs::create_dir_all(dir.join("sub").join("deep")).expect("nested dirs");
        std::fs::write(dir.join("a.bin"), [0u8; 10]).expect("file a");
        std::fs::write(dir.join("sub").join("b.bin"), [0u8; 20]).expect("file b");
        std::fs::write(dir.join("sub").join("deep").join("c.bin"), [0u8; 5]).expect("file c");
        assert_eq!(dir_size_bytes(&dir).expect("measured"), 35);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn dir_size_does_not_follow_symlinks() {
        let dir = temp_storage_dir("dir-size-symlink");
        std::fs::write(dir.join("real.bin"), [0u8; 10]).expect("real file");
        // A link back into the tree and one pointing outside it: neither
        // counts toward usage.
        std::os::unix::fs::symlink(dir.join("real.bin"), dir.join("link-in")).expect("symlink");
        std::os::unix::fs::symlink("/etc/hostname", dir.join("link-out")).expect("symlink");
        assert_eq!(dir_size_bytes(&dir).expect("measured"), 10);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
