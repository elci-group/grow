# Contributing to GROW

Thank you for your interest in contributing to GROW! We welcome contributions
from the community.

## Getting Started

1. Fork the repository
2. Clone your fork: `git clone https://github.com/YOUR_USERNAME/grow`
3. Create a branch: `git checkout -b feature/my-feature`
4. Make your changes
5. Run the quality audit: `scripts/quality-audit.sh`
6. Commit and push
7. Open a pull request

## Development Setup

```sh
# Install Rust (1.85+)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Build the project
cargo build

# Run tests
cargo test

# Run quality audit (format, clippy, tests, fract, amber)
scripts/quality-audit.sh
```

## Code Style

- Follow Rust formatting (`cargo fmt`)
- Zero warnings with Clippy (`cargo clippy --workspace --all-targets -- -D warnings`)
- Add tests for new functionality
- Document public APIs with rustdoc
- Keep commits focused and descriptive

## Testing

- Unit tests belong in the crate's `src/` under `#[cfg(test)]`
- E2E tests go in `crates/server/tests/`
- All tests must pass before merge
- Benchmarks are in `crates/runtime/benches/`

## Pull Request Process

1. Update documentation if you change behavior
2. Ensure the quality audit passes
3. Update `CHANGELOG.md` if applicable
4. Request review from maintainers
5. Squash fix-up commits if requested

## Reporting Issues

- Use GitHub Issues for bug reports and feature requests
- Include reproduction steps for bugs
- Include Rust version and OS info
- Search existing issues before creating new ones

## License

By contributing, you agree that your contributions will be licensed under the
MIT OR Apache-2.0 dual license.