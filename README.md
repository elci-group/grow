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
       |                     |                       |
       |              +------+------+                |
       |              | grow-mobile  |               |
       |              | (Android JNI)|               |
       |              +------+------+                |
       v                     v                       v
+---------------+     +---------------+     +---------------+
|  grow-git     |     |  grow-server  |     | grow-runtime  |
| (pure-gix     |     | resolve/serve |     | (wasm sandbox)|
|  repo loader) |     |               |     |               |
+---------------+     +---------------+     +---------------+
```

`grow-mobile` embeds core/git/server (static-only, no wasmtime) as the
on-device engine for Grow Browser Mobile in `mobile/`; the Chrome extension
in `extension/` resolves `grow://` URLs through `grow daemon`.

## Status: Milestone 2 — Local Registry & Storage (MVP track)

| Crate | Role | Tests |
|---|---|---|
| `crates/core` | `.grow/manifest.toml` schema, parsing, validation | 25 |
| `crates/git` | Repository loading via gix (pure-Rust git, incl. shallow https), HEAD verification | 18 + 1 network |
| `crates/runtime` | wasmtime + WASI preview1, CGI-style invocation, fuel-metered sandbox, storage grants | 10 |
| `crates/server` | `grow-server` daemon: static assets + WASM request handling + health/metrics + shared `grow://` resolve core | 22 |
| `crates/cli` | `grow run` / `grow serve` / `grow inspect` / `grow install` / `grow list` / `grow remove` / `grow daemon` | 10 |
| `crates/mobile` | Grow Browser engine (JNI): on-device `grow://` resolve, verify, static serve | 2 |

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

Modules receive stdio only by default — no filesystem, network, environment,
or arguments. Installed apps additionally receive a private storage directory
preopened at the module's filesystem root (see *Local app registry*).
Execution is bounded by:

- **Fuel metering**: ~1B instructions per invocation (deterministic)
- **Memory cap**: 256 MB linear memory per instance
- **Stack**: Safe stack limits enforced by wasmtime
- **Wall-clock timeout**: 5s per request (daemon-level)
- **Output cap**: 16 MB stdout/stderr captured
- **Request body limit**: 1 MB max body size
- **Header limits**: 64 headers max, 256B name, 8KB value
- **WASM feature restrictions**: memory64, multi-memory, threads disabled

## Local app registry

`grow install <repo>` registers an app in the local registry (`$GROW_HOME`,
else `$XDG_DATA_HOME/grow`, else `~/.local/share/grow`), recording the
verified commit, manifest identity, requested capabilities, and an approval
record. The source is a local repository path, a `file://` URL, or an
`https://` URL on github.com or gitlab.com; other schemes, other hosts, and
URLs carrying credentials are rejected. Remote clones are shallow and use
gix's pure-Rust https transport — no host `git` binary is required, which is
what lets the same loader run on Android. Cloned sources are kept at
`<registry>/cache/<name>/` and deleted by `grow remove <name>` — external
source repositories are never touched.

Apps declaring host capabilities (`network`, `camera`, `microphone`,
`clipboard`, `notifications`, or `filesystem = "full"`) must be installed
with `--approve-capabilities`. `grow run` and `grow serve` enforce the
install-time approval record for installed apps:

- HEAD moved since install:
  `app '<name>' has changed since install (was <commit>, now <commit>); run grow install again to re-verify`
- Host powers declared but not approved:
  `app '<name>' declares host capabilities that are not approved; reinstall with --approve-capabilities`

Each installed app gets a private storage directory at
`<registry>/apps/<name>/storage/`, preopened as the module's filesystem root
(`/`) with read/write access. The manifest's `storage.quota` is enforced
around every invocation; writes over quota fail with a clear quota error.
Approved host powers are only recorded — modules receive no host capabilities
beyond this storage directory yet.

Running or serving a plain repository path is dev mode: no approval check and
no storage grant (the module receives stdio only).

## Server endpoints

| Endpoint | Purpose |
|---|---|
| `GET /healthz` | Liveness probe — returns `ok\n` |
| `GET /metrics` | Basic metrics — app name, version, commit, `grow_up=1` |
| `GET /` | Static `index.html` from verified HEAD tree |
| `GET /path` | Static file if it exists in the repo, else WASM invocation |
| `POST /path` | WASM invocation (if runtime entry declared) |

## `grow://` URLs and the browser extension

`grow daemon` runs a resolver control plane on `127.0.0.1:7890` for the
Chrome extension in `extension/`. It answers
`GET /resolve?u=grow://<host>/<owner>/<repo>[/path]`: on first use the app
is cloned over https (`github.com` and `gitlab.com` only — credentials in
URLs are rejected), verified, installed into the registry, and served on a
per-app localhost port with the usual approval and storage policy; the
extension then navigates the tab to the returned URL.

```sh
./target/release/grow daemon
curl 'http://127.0.0.1:7890/resolve?u=grow://github.com/elci-group/octo-site'
# {"app":"octo-site","port":7901,"url":"http://127.0.0.1:7901/",...}
```

Resolving never updates an installed app: a moved remote keeps serving the
installed commit until `grow install <url>` is run again, matching the CLI
trust model. See `extension/README.md` for browser setup (Load unpacked),
the omnibox keyword (`grow` + Tab), link interception, and the popup.

## Grow Browser Mobile

`mobile/` is an Android browser where `grow://` is a first-class scheme —
no extension, no keyword, no desktop daemon. The address bar accepts
`grow://github.com/<owner>/<repo>` directly; the embedded `grow-mobile`
engine clones the repo over https (pure-Rust gix), verifies the HEAD commit
and manifest, and serves the site on a device-local port that the WebView
renders. Tabs, history, and bookmarks included; repositories are cloned,
verified, served, and rendered entirely on the end user's device.

```sh
# native .so for all ABIs, then the APK
cargo ndk -t arm64-v8a -t armeabi-v7a -t x86_64 \
  -o mobile/app/src/main/jniLibs build --release -p grow-mobile
cd mobile && ./gradlew :app:assembleDebug
adb install app/build/outputs/apk/debug/app-debug.apk
```

Milestone 1 serves static sites (all current demos); WASM execution on
device is the next milestone. See `mobile/README.md` for details.

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

# Registry flow: install once, then run/serve by name with the approval
# record and storage grant enforced
cargo run -p grow-cli -- install /tmp/hello-demo
cargo run -p grow-cli -- list
cargo run -p grow-cli -- serve hello-app
cargo run -p grow-cli -- remove hello-app
```

## License

MIT OR Apache-2.0
