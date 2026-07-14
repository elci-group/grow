# GROW

**Git Repository Oriented Web runtime** — applications are published as git
repositories and executed locally by a sandboxed Rust/WebAssembly runtime
instead of being deployed to a server.

## Architecture

```
+---------------+     +---------------+     +---------------+
|   grow-cli    |     |  grow-server  |     |  grow-core    |
|  (commands)   | --> |  (daemon)     | --> | (manifest)    |
+---------------+     +---------------+     +---------------+
                             |                       |
                             v                       v
                      +---------------+     +---------------+
                      |  grow-git     |     | grow-runtime  |
                      | (repo loader) |     | (wasm sandbox)|
                      +---------------+     +---------------+
```

## Status: Milestone 1 — Core Runtime (SOTA-Enhanced)

| Crate | Role | Tests |
|---|---|---|
| `crates/core` | `.grow/manifest.toml` schema, parsing, validation | 17 |
| `crates/git` | Repository loading via gix (pure-Rust git), HEAD verification | 11 |
| `crates/runtime` | wasmtime + WASI preview1, CGI-style invocation, fuel-metered sandbox | 4 |
| `crates/server` | `grow-server` daemon: static assets + WASM request handling + health/metrics | 1 e2e |
| `crates/cli` | `grow run` / `grow serve` / `grow inspect` | — |

**Security enhancements**: Path traversal protection, restricted WASM features
(memory64, multi-memory, threads disabled), request body/header limits,
concurrent invocation limits, 5s wall-clock timeout + fuel metering.

**Production features**: Graceful shutdown (Ctrl+C / SIGTERM), `/healthz` and
`/metrics` endpoints, structured logging via `tracing`, request body limits,
CI/CD pipeline via GitHub Actions.

**Quality**: Workspace tests, Clippy warnings-as-errors, formatting checks,
architecture indexing (Fract), dependency policy (Amber), automated
benchmarks (Criterion), man pages.

## Repository contract

A GROW application is a git repository containing `.grow/manifest.toml`:

```toml
[grow]
name = "example-app"
version = "1.0.0"

[runtime]
engine = "wasm"
entry = "main.wasm"

[network]
enabled = false

[storage]
quota = "500mb"

[permissions]
filesystem = "restricted"
camera = false
microphone = false
```

Repositories without a `[runtime]` section are served as static sites.

## WASM module contract (M1)

The entry module is a WASI preview1 command module. Each HTTP request runs a
fresh instance, CGI-style:

- **stdin**: `METHOD PATH\n`, then `Header: value\n` lines, a blank line, then the body
- **stdout**: `STATUS REASON\n` (e.g. `200 OK`), then headers, a blank line, then the body

Modules receive stdio only — no filesystem, network, environment, or
arguments. Execution is bounded by:

- **Fuel metering**: ~1B instructions per invocation (deterministic)
- **Memory cap**: 256 MB linear memory per instance
- **Stack**: Safe stack limits enforced by wasmtime
- **Wall-clock timeout**: 5s per request (daemon-level)
- **Output cap**: 16 MB stdout/stderr captured
- **Request body limit**: 1 MB max body size
- **Header limits**: 64 headers max, 256B name, 8KB value
- **WASM feature restrictions**: memory64, multi-memory, threads disabled

## Server endpoints

| Endpoint | Purpose |
|---|---|
| `GET /healthz` | Liveness probe — returns `ok\n` |
| `GET /metrics` | Basic metrics — app name, version, commit, `grow_up=1` |
| `GET /` | Static `index.html` from verified HEAD tree |
| `GET /path` | Static file if it exists in the repo, else WASM invocation |
| `POST /path` | WASM invocation (if runtime entry declared) |

## Build and test

```sh
cargo build
cargo test
```

Run benchmarks:

```sh
cargo bench --workspace
```

Inspect a repository:

```sh
grow inspect /path/to/app
```

## Quality audit

Run the full quality audit (format, clippy, tests, fract, amber, kaptaind):

```sh
scripts/quality-audit.sh
```

## Roadmap

The executable SOTA roadmap lives in [docs/roadmap.md](docs/roadmap.md). It
tracks 30 development domains, phase gates, immediate backlog, and current
non-SOTA gaps.

The MVP contract lives in [docs/mvp.md](docs/mvp.md). It defines the first
release bar: local install/run/serve/remove, durable approvals, enforced
storage quota, useful diagnostics, and repeatable release gates.

## CI/CD

GitHub Actions workflow runs the full quality audit on every push and pull
request. See `.github/workflows/ci.yml`.

## Demo

```sh
# Build the example app's WASM module from its WAT source
cargo run -p grow-runtime --example build-hello-wasm

# A GROW app must be a git repository
cp -r examples/hello-app /tmp/hello-demo
git -C /tmp/hello-demo init -b main
git -C /tmp/hello-demo add -A
git -C /tmp/hello-demo -c user.email=demo@grow -c user.name=demo commit -m "hello-app"

# Serve it (http://127.0.0.1:7870) — supports Ctrl+C graceful shutdown
cargo run -p grow-cli -- serve /tmp/hello-demo
curl http://127.0.0.1:7870/            # static index.html
curl http://127.0.0.1:7870/healthz      # liveness probe
curl http://127.0.0.1:7870/metrics      # basic metrics
curl http://127.0.0.1:7870/api/hello   # handled by the WASM module

# One-shot execution
cargo run -p grow-cli -- run /tmp/hello-demo
```

## License

MIT OR Apache-2.0
