# Security Policy

## Supported Versions

| Version | Supported          |
| ------- | ------------------ |
| 0.1.x   | :white_check_mark: |

## Reporting a Vulnerability

If you discover a security vulnerability, please report it responsibly:

1. **Email**: security@grow.dev (or project maintainer)
2. **Do not** open a public issue until the vulnerability is fixed
3. Include:
   - Description of the vulnerability
   - Steps to reproduce
   - Potential impact assessment
   - Suggested fix (if any)

## Response Process

1. We will acknowledge receipt within 48 hours
2. We will investigate and provide a timeline for fix within 7 days
3. We will coordinate disclosure with you before public release
4. We will credit you in the security advisory (unless you prefer anonymity)

## Security Measures in Place

- Path traversal protection on all filesystem reads
- WASM sandbox with restricted features (no memory64, multi-memory, threads)
- Fuel metering to prevent infinite loops
- Request body and header size limits
- Output limits on captured stdout/stderr
- Wall-clock timeouts per request
- No filesystem/network access granted to WASM modules by default

## Dependencies

Security audits are run via:

```sh
cargo audit  # via cargo-audit
cargo deny check advisories  # via cargo-deny
```

These checks are part of the CI pipeline.