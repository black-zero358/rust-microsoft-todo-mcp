# M1 validation ledger

Date: 2026-11-06. Candidate source: the development commit containing this ledger. Environment: Linux x86_64 cloud workspace, Intel Xeon Platinum 8573C, Rust 1.99.1, SQLite bundled with rusqlite 0.37.0. All fixtures are synthetic; no real Microsoft tasks or production identities were used.

## Executed locally

| Check | Result |
| --- | --- |
| `cargo fmt --all -- --check` | Passed |
| `cargo clippy --all-targets --locked -- -D warnings` | Passed |
| `cargo test --locked` | 50 passed, 0 failed |
| `cargo build --release --locked` | Passed |
| `node --check src/admin.js` | Passed |
| `python scripts/smoke.py` | Health, unauthenticated 401, MCP initialize, tools/list and tools/call passed |
| `cargo run --release --locked --example benchmark` | 10,000 synthetic tasks, 100 searches per query |
| `git diff --check` | Passed |

Release-binary SHA-256 in this environment: `3d8572ff856e98b8605c72dd6f6c2dc87a0ab87a46c7607621882824f6bbb9e4`. Rebuilds in another environment may produce a different digest.

The test suite covers signed JWT rejection paths, device polling/slowdown/expiry, refresh rotation, early Graph 401 refresh, account revocation and reauthorization fences, cross-account cache/operation isolation, FTS and short Chinese queries, HTML extraction, paging/staging/restart recovery, delta tombstones and 410 replacement, malformed provider responses, request URL confinement, full MCP routing, uncertain writes and same-key replay. Review regressions include dot-segment resource IDs, deleted-list operation replay and equivalent percent-encoded continuation paths.

## Measurements

Release process with an empty account catalog, using the local smoke probe:

- Process launch to successful health response: **18.455 ms** (includes probe scheduling).
- Idle resident memory after MCP smoke: **11,156 KiB**.
- Discovered tools: **26**.
- Full `tools/list` response: **22,640 bytes**; empty `list_accounts` tool response: **162 bytes**.
- Graph requests in the offline probe: **0**.

Database fixture: one synthetic account/list, 10,000 tasks, Chinese/English title/body text, 25-result pages. Database open: 1.185 ms; bulk fixture publication/indexing: 574.875 ms. Timings measure cache search and exclude JSON serialization; wire bytes include the full JSON-RPC envelope, structured content and text copy with request ID 1.

| Query | p50 | p95 | Full wire bytes | Graph requests |
| --- | --- | --- | --- | --- |
| `中文任务` | 2.085 ms | 3.680 ms | 11,784 | 0 |
| `项目` | 0.097 ms | 0.274 ms | 11,784 | 0 |
| `Project 9876` | 0.979 ms | 1.708 ms | 1,180 | 0 |

These are reproducible fixture measurements, not performance commitments. Run the supplied probes on the deployment hardware for its baseline.

## CI and remaining acceptance

The workflow repeats format/lint/tests/release checks, builds the Docker image, runs the release smoke and benchmark, and checks container health plus unauthenticated rejection. The local workspace has no Docker engine, so container execution is delegated to CI and must be checked on the exact PR commit.

Live acceptance still requires the operator's deployment and consent:

1. HTTPS reverse proxy and OAuth provider configuration; real ChatGPT PKCE login and natural refresh.
2. Personal and organizational Microsoft sign-in, including tenant consent policies.
3. Live list/task/step/link CRUD, reminder and recurrence behavior, nullable field clearing, and phone-to-cache delta updates.
4. Restart with real authorization and two-account isolation/disconnect.

Use the dedicated disposable-list procedure in [deployment](deployment.md). M1 remains an implementation candidate until these live checks pass. Attachment tools are M2. License selection awaits the project owner's decision.
