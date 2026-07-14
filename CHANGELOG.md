# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- SOTA development roadmap implementation across 30 domains
- MVP contract defining supported scope, excluded scope, capability rules,
  storage rules, CLI surface, failure messages, test matrix, and release gate
- Version alignment check for `VERSION` and workspace package metadata
- 17 unit tests for `grow-core` manifest parsing and validation
- 11 unit tests for `grow-git` repository loading and HEAD verification
- 4 unit tests for `grow-runtime` WASM sandbox execution
- 1 end-to-end test for `grow-server` HTTP request handling
- Criterion benchmarks for WASM compilation, invocation, and response parsing
- Request validation: body limits (1MB), header limits (64 headers, 256B name, 8KB value)
- WASM feature restrictions: memory64, multi-memory, threads disabled
- Graceful shutdown with Ctrl+C and SIGTERM signal handling
- `/metrics` endpoint for basic health metrics
- Enhanced CLI with verbose help text and improved error messages
- Comprehensive README with architecture diagram and security documentation
- `CONTRIBUTING.md` with development setup and PR guidelines
- GitHub Actions CI/CD pipeline with quality audit, tests, and clippy
- `SECURITY.md` with vulnerability reporting process
- `cargo-deny` configuration for license compliance and vulnerability scanning
- Dockerfile for containerized server deployment
- Static asset optimization: compression-ready headers, ETag support framework
- MSRV policy documentation (Rust 1.85+)
- Request timeout and circuit breaker patterns (5s wall-clock + fuel metering)
- Cross-platform build support documentation (Linux, macOS, Windows)

### Changed
- Synced workspace version with `VERSION` file (0.1.18)
- Improved `grow inspect` output with structured formatting and permission details
- Enhanced error messages with context (repository paths, file names)
- Updated server to use `tokio` with `signal` feature for graceful shutdown

### Security
- Hardened WASM sandbox with restricted feature flags
- Path traversal protection on all filesystem reads
- Input validation for request headers, body, and paths
- Output limits on captured stdout/stderr (16MB)
- Concurrent invocation limits (32 max)

## [0.1.3] - 2025-01-13

### Fixed
- Version synchronization between `Cargo.toml` and `VERSION` file

## [0.1.2] - 2025-01-13

### Added
- Initial Milestone 1 implementation
- Core manifest parsing (`grow-core`)
- Git repository loading via `gix` (`grow-git`)
- WASM runtime with `wasmtime` + WASI preview1 (`grow-runtime`)
- Local server with static assets and WASM routing (`grow-server`)
- CLI with `inspect`, `run`, `serve` commands (`grow-cli`)
- Quality audit script (`scripts/quality-audit.sh`)
- Manual pages (`docs/man/`)
- Example application (`examples/hello-app`)
- Kaptaind, Fract, Amber, and Deliver configuration

[Unreleased]: https://github.com/sal/grow/compare/v0.1.3...HEAD
[0.1.3]: https://github.com/sal/grow/compare/v0.1.2...v0.1.3
[0.1.2]: https://github.com/sal/grow/releases/tag/v0.1.2
