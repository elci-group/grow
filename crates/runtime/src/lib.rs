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
//! * the module receives stdio only — no filesystem, network, env, or args.

use wasmtime::{Config, Engine, Linker, Module, Store, StoreLimits, StoreLimitsBuilder, Trap};
use wasmtime_wasi::p1::{self, WasiP1Ctx};
use wasmtime_wasi::p2::pipe::{MemoryInputPipe, MemoryOutputPipe};
use wasmtime_wasi::{I32Exit, WasiCtxBuilder};

/// Stack size limit per module instance (8 MB).
pub const MAX_STACK_BYTES: usize = 8 * 1024 * 1024;

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
        let engine = Engine::new(&config).map_err(|e| RuntimeError::Engine(e.to_string()))?;
        let module = Module::new(&engine, wasm).map_err(|e| RuntimeError::Engine(e.to_string()))?;
        Ok(Self { engine, module })
    }

    /// Invoke the module's `_start` with `stdin` as its standard input and
    /// return captured stdout. Synchronous and CPU-bound — callers in async
    /// contexts should use `spawn_blocking` plus a timeout.
    pub fn run_raw(&self, stdin: &[u8]) -> Result<Vec<u8>, RuntimeError> {
        let stdout = MemoryOutputPipe::new(MAX_OUTPUT_BYTES);
        let stderr = MemoryOutputPipe::new(MAX_OUTPUT_BYTES);

        let mut builder = WasiCtxBuilder::new();
        builder
            .stdin(MemoryInputPipe::new(stdin.to_vec()))
            .stdout(stdout.clone())
            .stderr(stderr.clone());
        // Deliberately no preopened dirs, env, or args: stdio only.
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
            Ok(()) => Ok(stdout.contents().to_vec()),
            Err(err) => {
                if let Some(exit) = err.downcast_ref::<I32Exit>() {
                    if exit.0 == 0 {
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

    /// Invoke the module and parse its stdout as a [`Response`].
    pub fn invoke(&self, request: &Request) -> Result<Response, RuntimeError> {
        parse_response(&self.run_raw(&request.frame())?)
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
            .invoke(&Request {
                method: "GET".into(),
                path: "/hello".into(),
                headers: vec![("Host".into(), "localhost".into())],
                body: Vec::new(),
            })
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
        let err = runtime.run_raw(&[]).unwrap_err();
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
        let err = runtime.run_raw(&[]).unwrap_err();
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
}
