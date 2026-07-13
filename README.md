# GROW

**Git Repository Oriented Web runtime** — applications are published as git
repositories and executed locally by a sandboxed Rust/WebAssembly runtime
instead of being deployed to a server.

## Status: Milestone 1 — Core Runtime

| Crate | Role |
|---|---|
| `crates/core` | `.grow/manifest.toml` schema, parsing, validation |
| `crates/git` | Repository loading via gix (pure-Rust git), HEAD verification |
| `crates/runtime` | wasmtime + WASI preview1, CGI-style invocation, fuel-metered sandbox |
| `crates/server` | `grow-server` daemon: static assets + WASM request handling on localhost |
| `crates/cli` | `grow run` / `grow serve` |

Later milestones (browser extension, `grow://`, distributed object cache,
registry) are not implemented yet.

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
arguments. Execution is bounded by fuel metering and a 256 MB linear-memory
cap; requests also have a 5 s wall-clock timeout in the daemon.

## Build and test

```sh
cargo build
cargo test
```

Inspect the exact commit and manifest configuration GROW will use:

```sh
grow inspect /path/to/app
```

## Engineering checks

Kaptaind monitors the working tree and gates automated commits on the full
workspace test suite. Run the same local quality audit used for MVP work with:

```sh
scripts/quality-audit.sh
```

The audit covers formatting, warnings-as-errors Clippy, workspace tests,
Fract architecture indexing, Amber dependency policy, and Kaptaind release
readiness. Amber must be rerun after dependency or Cargo manifest changes;
Fract should be rerun after structural changes.

Manual pages for the binaries are in `docs/man/`.

## Demo

```sh
# Build the example app's WASM module from its WAT source
cargo run -p grow-runtime --example build-hello-wasm

# A GROW app must be a git repository
cp -r examples/hello-app /tmp/hello-demo
git -C /tmp/hello-demo init -b main
git -C /tmp/hello-demo add -A
git -C /tmp/hello-demo -c user.email=demo@grow -c user.name=demo commit -m "hello-app"

# Serve it (http://127.0.0.1:7870)
cargo run -p grow-cli -- serve /tmp/hello-demo
curl http://127.0.0.1:7870/            # static index.html
curl http://127.0.0.1:7870/api/hello   # handled by the WASM module

# One-shot execution
cargo run -p grow-cli -- run /tmp/hello-demo
```
