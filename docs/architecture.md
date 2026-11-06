# Architecture and invariants

## Modules

- `config`: validates configuration before listening; enforces HTTPS and strict local-only development mode.
- `auth`: verifies inbound owner JWTs and independently manages Microsoft device sign-in and encrypted refresh tokens.
- `graph`: bounded authenticated Graph transport, URL validation before credentials, safe-read retry rules.
- `db`: SQLite account isolation, indexed search, staged synchronization and durable operation intent ledger.
- `sync`: per-account serialized list/task delta scans, restartable page progress and atomic stream publication.
- `tools`: strict tool schemas, explicit write account selection, field validation, operation idempotency and read-only outcome reconciliation.
- `server`: official Rust MCP SDK Streamable HTTP, per-request authorization, bounded request/response handling, management routes and admission control.

Graph is authoritative. Cache queries expose synchronization state. Live entity reads and all writes use Graph. Successful writes are durably recorded before refreshing the cache; cache refresh failure is returned separately from write success. Completing a repeating task refreshes Graph state instead of inventing the next occurrence locally.

## Reliability boundaries

Every mutation is identified by account and operation key, bound to a canonical request fingerprint. Its intent is committed before the HTTP request. A repeated key returns the same operation; changed content with the same key is rejected. Timeout, broken response or ambiguous 5xx becomes `unknown`; the server does not automatically submit it again. Startup recovers pending intents as unknown.

Read-only reconciliation checks the addressed resource for updates/deletions. Unknown creates list matching candidates for operator/client review. Similar task text is not used as an automatic idempotency identifier. Post-start HTTP disconnection does not cancel the admitted tool's result bookkeeping. Unexpected process death still leaves a recoverable durable intent.

All cache keys, cursors, credentials and operation keys are scoped by account. Account generations fence asynchronous commits after disconnect/re-authorization. Local removal cascades through cache, indexes and cursors. In-flight requests already sent to Microsoft can finish after removal; remote grant revocation is a separate Microsoft action.

Delta changes and the next continuation are staged in one transaction. Only a completed stream publishes the changes and final delta token. Resume uses durable next links. A 410 discards the invalid staging chain and starts an authoritative replacement; old visible data stays available with stale/error status until publication. Tombstones remove records and list deletion removes dependent tasks and streams.

## Security

MCP authorization and Microsoft delegated permissions are distinct trust boundaries. The MCP OAuth audience is the exact public `/mcp` resource. RS256 signatures, issuer, audience, expiry/not-before, owner subject and scope are checked on each request. JWKS retrieval is bounded and pinned to the configured issuer origin. Admin APIs require a separate random bearer secret and reject foreign origins. No browser credential persistence or cookies are used.

Microsoft refresh tokens are AES-256-GCM encrypted with random nonces and account-bound additional authenticated data. Access tokens are held in memory. The volume and backups still contain task text and must remain private. Graph credentials are sent only to the pinned Graph origin, never to arbitrary pagination URLs; HTTP redirects are disabled. Tool descriptions treat task content as untrusted data.

## Lessons from rust-email-mcp

Reference snapshot: `rust-email-mcp` commit `1f1a0b6d52ea2a92b8007758b5b55cb306766849` was inspected during this implementation. Useful practices retained:

- Validate authorization before data access; keep identity in HTTP request context rather than trusting session possession.
- Persist irreversible write intent before contacting the provider and preserve unknown outcomes.
- Budget the full JSON-RPC response, including its text copy and actual request ID.
- Separate cache freshness from process health and from successful remote writes.
- Test with synthetic local providers and verify restart/race paths, not just happy-path helpers.

Changes made for this project:

- No borrowed IMAP protocol layers, MIME pipeline or vendored patches. Graph transport and domain-specific rules are small separate modules.
- Multi-account writes require explicit account IDs from day one; there is no implicit legacy-account write behavior.
- Chinese search and persistent indexing are part of the first implementation, with short-query fallback.
- OAuth setup is documented as a deployment dependency; local bearer smoke tests and live ChatGPT acceptance are tracked separately.
- A compact invariant-focused test suite replaces duplicate scenarios. CI gates formatting, lint, tests, release compilation and Docker build on the same source.

## Acceptance map

| Invariant | Automated checks |
| --- | --- |
| JWT signature, expiry, issuer/resource/subject/scope | `auth::jwt_tests` |
| HTTP auth, discovery, MCP initialization/tools, Origin and budgets | `server::tests` |
| Graph URL confinement, retry and response-size rules | `graph::tests` |
| Account/default isolation, tombstones, staged recovery and revoke fences | `db::tests`, `sync::tests` |
| Schema/field validation and durable write replay | `tools::tests`, `tools::lifecycle_tests` |

Live provider/client behavior and deployment validation are tracked in [validation](validation.md).
