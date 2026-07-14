# GROW MVP Contract

This document defines the minimum viable product for GROW. The MVP is not the
full SOTA roadmap; it is the smallest release that is safe, understandable,
installable, testable, and useful for a developer trying local Git-based apps.

## MVP Goal

A user can install `grow`, inspect a GROW app, understand its requested powers,
run it locally, serve it locally, and remove it without editing project internals
or reading Rust errors.

The target flow is:

```sh
grow inspect examples/hello-app
grow install examples/hello-app
grow list
grow serve hello-app
grow remove hello-app
```

## Supported In MVP

- Local Git repositories with a committed `.grow/manifest.toml`.
- Static-only repositories with no `[runtime]` section.
- WASM repositories using the current WASI preview1 command-module contract.
- `grow inspect` for app identity, commit, runtime, entry point, permissions,
  storage request, and warnings.
- `grow run` for one-shot local execution.
- `grow serve` for localhost HTTP serving.
- Per-app install metadata stored locally.
- Durable approval records for requested host capabilities, tied to app name,
  verified commit, approval time, and declared capabilities.
- Per-app storage directories with enforced quota.
- Health and metrics endpoints for the local server.
- Clear user-facing errors for common failure cases.

## Not Supported In MVP

- Public marketplace or app registry.
- Multi-user server mode.
- Remote app trust beyond a narrow Git fetch/install path.
- Camera, microphone, clipboard, notifications, or broad filesystem access.
- Background auto-update.
- Distributed deployment.
- SOTA performance claims without published benchmarks.

## Runtime Contract

The MVP keeps the current WASM contract:

- Entry modules are WASI preview1 command modules.
- Each HTTP request runs in a fresh sandboxed instance.
- `stdin` receives `METHOD PATH`, headers, a blank line, then the request body.
- `stdout` returns `STATUS REASON`, headers, a blank line, then the response
  body.
- Modules receive no filesystem, network, environment, or arguments unless a
  future capability explicitly grants them.

## Capability Contract

Capabilities must be denied by default.

For any capability to be usable:

1. The app declares it in `.grow/manifest.toml`.
2. `grow inspect` displays it clearly.
3. `grow install` records explicit user approval.
4. Runtime checks the approval record before granting access.
5. The decision is tied to app identity and verified Git state.

MVP should implement the approval record before enabling risky host powers.

## Storage Contract

MVP storage is scoped per app:

- Each installed app gets a private data directory.
- The manifest `storage.quota` value is enforced.
- Writes over quota fail with a clear quota error.
- Removing an app removes or archives its app-scoped metadata and storage by
  explicit command behavior.

## Required CLI Surface

| Command | MVP behavior |
|---|---|
| `grow inspect <repo-or-app>` | Show app identity, commit, runtime, entry point, requested permissions, storage quota, and warnings |
| `grow run <repo-or-app>` | Execute one request locally and print the response |
| `grow serve <repo-or-app>` | Serve the app on localhost |
| `grow install <repo>` | Register app metadata, verified commit, manifest, and approval record |
| `grow list` | Show installed apps and versions |
| `grow remove <app>` | Remove installed app metadata and app-scoped state according to policy |

## Required Failure Messages

MVP must provide plain, actionable errors for:

- Missing `.grow/manifest.toml`.
- Invalid manifest syntax.
- Unsupported manifest field or runtime engine.
- Missing WASM entry file.
- Permission declared but not approved.
- Storage quota exceeded.
- WASM timeout or fuel exhaustion.
- Malformed WASM module.
- Invalid repository or missing Git commit.
- Port already in use.

## MVP Test Matrix

Before tagging MVP, the suite must cover:

- Valid static app.
- Valid WASM app.
- Missing manifest.
- Invalid manifest.
- Missing runtime entry.
- Path traversal attempt.
- Oversized request body.
- Oversized or invalid headers.
- Malformed WASM.
- Infinite-loop WASM.
- Excessive module output.
- Concurrent requests.
- Storage quota boundary.
- Capability requested but unapproved.
- Install, list, serve, and remove lifecycle.

## Release Gate

The MVP release gate is:

```sh
deliver --spec deliver.toml --strict
scripts/quality-audit.sh
```

The release must also include:

- Version alignment across `VERSION`, `Cargo.toml`, and local workspace
  packages in `Cargo.lock`.
- Updated `CHANGELOG.md`.
- Binary artifact checksums.
- Security advisory status.
- Known limitations documented in this file or `docs/roadmap.md`.
