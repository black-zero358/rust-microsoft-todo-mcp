# M1 adversarial review

Review date: 2026-11-06. This pass examines the existing M1 implementation with synthetic local providers and persistent SQLite fixtures. See [coverage](M1-COVERAGE.md) for the remaining live acceptance steps.

## Fixed findings and regression evidence

| Area | Counterexample | Change and evidence |
| --- | --- | --- |
| Database ownership | A second process opens the live database and changes active pending writes to unknown | Lifetime OS database-file lock before recovery; both same-process and child-process tests |
| Migration | Index rebuild fails after startup recovery changed operations | Schema/index/recovery in one transaction; corrupted legacy row test verifies rollback |
| Delta resurrection | Tombstone followed by a partial entity update restores stale fields or deleted list children | Track reset_base in staged records; task/list resurrection tests |
| Snapshot replacement | Full list rebuild preserves fields omitted by the authoritative snapshot | Replace base properties on completed rebuild; regression retains valid child tasks |
| Sync loops | Equivalent or repeated continuation recurs after restart/page budget | Durable visited-cursor history, canonical URLs and legacy-history normalization; bad page never stages |
| Sync status | Last-success plus ready reports complete while a new list lacks its task stream | Require published list and every cached-list task cursor; report incomplete_streams |
| JWT boundary | Array-valued iss passes library validation, malformed nbf is silently ignored | Typed exact issuer and not-before checks; signed-token rejection regressions |
| OAuth timing | Slow identity lookup connects an expired device session or extends token lifetime | Capture deadlines before provider request; recheck login expiry before commit; delay tests |
| Polling | Provider interval over 60 seconds is shortened, or repeated slow_down stops increasing | Preserve provider interval and add five seconds per slowdown; server and management JS tests |
| Authorization race | Disconnect or parallel login occurs during final identity lookup/admission | Serialize login admission/revocation; generation checks before subsequent Graph reads and retries |
| JWKS outage | Failed fetch is cached as unauthorized instead of unavailable | Preserve auth_unavailable during retry cooldown; HTTP maps it to 503 |
| Config/header handling | Unusable bearer secret accepted; case variation rejected; duplicate credentials ambiguous | Validate generated-token alphabet/length, case-insensitive Bearer scheme, reject duplicate headers; UUID normalization |
| Recovery response budget | Large successful/unknown operation cannot be inspected because replay itself exceeds MCP budget | Complete durable record with bounded inspection summaries and preserved IDs; oversized recovery test |
| Create reconciliation | Provider normalization hides a candidate; collection scan grows without bounds | Match title/displayName, bounded page/candidate views, validated next cursor and explicit manual verification |
| Update reconciliation | Missing property is mistaken for an explicitly cleared null | Require observed property; null-readback regression |
| Async mutation response | HTTP 202 is marked succeeded before completion | Record unknown; 202/408 no-replay regression |
| Actual lost response | Remote write succeeds, then its 2xx body breaks | POST/PATCH/DELETE fixtures commit state before ConnectionReset; reconcile and replay verify exactly one mutation |
| Admission/shutdown | Slow bodies or disconnected clients keep unbounded work alive; normal stop drops bookkeeping | 64-request and 32-tool admission, 15s body deadline, tracked detached work and shutdown drain tests |
| Wire budget | Full response copy doubles allocation while checking a large result | Counting serializer includes escaped text, duplicate content and actual request ID without cloning response |
| Admin UI | Duplicate clicks create multiple logins; stale response replaces newer/private view; network failure invisible | Disable active button, response versions and token snapshot, explicit error message; six Node DOM-fixture tests |

## Operational changes

- SQLite schema is now version 3. Preserve a pre-upgrade volume backup with its matching image; older binaries reject the newer schema. Migration rollback is transactional.
- Database and SQLite sidecar files use owner-only permissions on Unix. Backups still need equivalent access controls because task content is plaintext.
- Use one service instance per database on a local filesystem supporting advisory locks. Compose already allows a 45-second stop grace period.
- A stopped or crashed process leaves uncertain operations recoverable using the original operation key. The reconciliation interface never submits a new write.
- Continue using the exact same client operation key after transport failure. For creates, inspect every returned candidate page before deciding whether a separate new operation is necessary.

## Verification layers

Rust unit/integration tests exercise real local TCP HTTP transport for Graph and OAuth; they do not require Microsoft credentials. The administration JS tests use Node's test runner and a minimal DOM fixture. Release smoke exercises the running MCP endpoint. GitHub Actions repeats checks and runs the actual Docker image. Production OAuth, tenant policy, recurring-task behavior and phone synchronization are tracked separately in the M1 coverage checklist.
