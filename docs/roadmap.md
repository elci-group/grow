# GROW SOTA Roadmap

This roadmap turns the 30-domain SOTA target into executable work. It is
ordered by dependency: product contract and security gates come first, followed
by runtime capability, operations, ecosystem, and external validation.

## Execution Rules

- Every milestone must keep `cargo fmt`, Clippy, workspace tests, and
  `cargo deny check` green.
- Security-sensitive runtime changes need adversarial tests, not only happy
  path tests.
- New public behavior requires README or manual page updates in the same
  change.
- Features that grant host access must be denied by default, declared in the
  manifest, approved by policy, and observable at runtime.
- SOTA status is not self-certified. Each release needs a comparison matrix
  against at least three adjacent runtimes or platforms.

## Phase Plan

| Phase | Goal | Exit gate |
|---|---|---|
| P0: Baseline | Freeze the current contract and quality gates | Roadmap, threat model, release checklist, and supply-chain checks are mandatory |
| P1: Harden | Close M1 security and reliability gaps | Storage quota, capability denial tests, timeout tests, and malformed WASM tests pass |
| P2: Complete | Add install, approvals, and repository lifecycle | Local install flow, approval records, remote fetch, and upgrade/rollback are implemented |
| P3: Operate | Make GROW observable and supportable | Structured metrics, logs, health, load tests, and incident runbooks exist |
| P4: Lead | Prove differentiated SOTA behavior | Public benchmarks, compatibility matrix, and external review are published |

## Thirty Domains

| # | Domain | SOTA target | Acceptance gate | First milestone |
|---|---|---|---|---|
| 1 | Product contract | A crisp, testable definition of what a GROW app is | Contract doc and compatibility tests | Publish app/repo/runtime contract |
| 2 | Versioning | Semver, MSRV, CLI, crate, and manifest versions stay aligned | Automated version consistency check | Add release checklist and version test |
| 3 | CI quality | CI is deterministic and fail-fast for required gates | Format, Clippy, tests, deny, and audit required | Make deliver include roadmap and deny checks |
| 4 | Test strategy | Unit, integration, e2e, fuzz, and adversarial tests cover risk | Coverage map linked to domains | Add adversarial runtime/server tests |
| 5 | Documentation | Docs explain current behavior, not aspirational behavior | Docs updated with each public change | Link roadmap and clarify current gaps |
| 6 | Manifest schema | Manifest is stable, versioned, and forward-compatible | Schema fixtures and compatibility tests | Add manifest version field policy |
| 7 | Capability security | Host capabilities are explicit, denied by default, and audited | Approval and enforcement tests | Implement permission approval records |
| 8 | WASM isolation | Wasmtime config is hardened and regression-tested | Feature denial and fuel tests | Add malformed/infinite-loop WASM tests |
| 9 | Request security | HTTP limits resist abuse and preserve correctness | Boundary tests for method, path, headers, body | Expand server adversarial test matrix |
| 10 | Secret handling | Secrets never enter manifests, logs, argv, or module env by default | Secret redaction tests | Add secret policy doc and log tests |
| 11 | Supply chain | Licenses, advisories, duplicate deps, and sources are controlled | `cargo deny check` required | Reduce duplicate dependency warnings |
| 12 | Vulnerability response | Security issues have repeatable triage and release flow | SECURITY runbook and advisory template | Add vulnerability response checklist |
| 13 | Storage | Quotas are enforced with deterministic failure behavior | Quota tests and persistence tests | Enforce configured storage quota |
| 14 | Networking | Network access is off by default and policy scoped | Deny/allow tests per destination | Design outbound network capability model |
| 15 | Runtime APIs | Request/response ABI is stable and language-neutral | ABI conformance suite | Add ABI fixtures for multiple modules |
| 16 | Performance | Cold start, warm invoke, and static serving are benchmarked | Published baseline and regression budget | Add benchmark threshold reporting |
| 17 | Caching | Verified Git trees, compiled modules, and static assets cache safely | Cache invalidation tests | Add compiled-module cache design |
| 18 | Concurrency | Load does not break isolation, fairness, or limits | Concurrent stress tests | Add runtime/server stress suite |
| 19 | Reliability | Failures are bounded, classified, and recoverable | Error taxonomy and recovery tests | Normalize public error types |
| 20 | Observability | Operators can debug apps without exposing sensitive data | Metrics/logging contract and tests | Expand `/metrics` and structured spans |
| 21 | Operations | Local daemon lifecycle is boring and inspectable | Install, service, and runbook docs | Add systemd/user service guidance |
| 22 | Deployment | Binaries, containers, and packages are reproducible | Release artifact checksums | Add release packaging workflow |
| 23 | Distribution | Users can install and update predictably | Signed releases and upgrade tests | Define install/update channels |
| 24 | Identity | App identity is tied to verified Git state and trust policy | Signature/provenance tests | Design signed repository trust model |
| 25 | Multi-tenancy | Multiple apps cannot interfere with each other | Isolation tests across app instances | Add per-app state directories |
| 26 | Developer experience | Common app workflows are short, explicit, and debuggable | CLI golden tests and examples | Add `grow new` or example scaffolding plan |
| 27 | Ecosystem | Templates, examples, and SDKs make app authoring practical | Example apps and ABI SDK tests | Add Rust and TinyGo authoring examples |
| 28 | Edge | Runtime can operate under constrained local/edge conditions | Resource profile benchmarks | Define minimum resource profile |
| 29 | Compliance | Licenses, SBOM, provenance, and retention are auditable | SBOM and license report in release | Add SBOM generation to release flow |
| 30 | SOTA validation | Claims are benchmarked against real alternatives | Public scorecard and external review | Publish comparison matrix |

## Immediate Execution Backlog

1. Land the baseline documentation and make `deliver` require it.
2. Keep `cargo deny check` as a required local completion gate.
3. Add adversarial tests for traversal, oversized headers, timeout behavior,
   infinite-loop WASM, malformed WASM, and concurrent invocations.
4. Implement storage quota enforcement before adding broader host capabilities.
5. Design install-time approval records before enabling filesystem, network,
   camera, microphone, clipboard, or notification capabilities.
6. Add a release checklist that verifies version alignment, changelog links,
   artifacts, checksums, SBOM, and security advisory state.

## Current Non-SOTA Gaps

- Install clones local paths and `file://` URLs only; network remotes,
  upgrade, rollback, and signing are not implemented.
- Fract and Amber are fail-fast in the local audit script, but advisory in the
  GitHub workflow until their installation story is pinned.
- There is no published benchmark or comparison matrix yet.
