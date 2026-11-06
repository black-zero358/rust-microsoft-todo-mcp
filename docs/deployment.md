# VPS deployment

## Prerequisites

- Linux VPS with Docker and the Compose plugin, at least 512 MiB RAM recommended.
- A domain with DNS pointing to the VPS and inbound HTTPS allowed.
- A Microsoft app registration and an OAuth provider for MCP client login. These are separate registrations. Follow [authentication](authentication.md).
- Operator access to the VPS. Do not send credentials through task or issue comments.

## Configure

```sh
git clone https://github.com/black-zero358/rust-microsoft-todo-mcp.git
cd rust-microsoft-todo-mcp
cp .env.example .env
chmod 600 .env
openssl rand -hex 32  # use one result for TODO_ADMIN_TOKEN
openssl rand -hex 32  # use a different result for TODO_ENCRYPTION_KEY
```

Set the variables in `.env` as described in [authentication](authentication.md). Keep the encryption key unchanged across restarts and upgrades. Changing it makes stored refresh tokens unreadable and requires account sign-in again. The administrator token is separate from MCP client access tokens. Do not set `TODO_DEV_MCP_TOKEN` in Docker or production.

```sh
docker compose up -d --build
docker compose logs --tail 50 todo
curl --fail http://127.0.0.1:8080/healthz
```

The health endpoint reports process/configuration readiness. Account connection and synchronization status are shown separately in the management page.

Run Caddy on the VPS using `deploy/Caddyfile`, replacing `todo.example.com`. It obtains and renews HTTPS certificates. If using another reverse proxy, preserve `Authorization`, `Host`, `Origin` and MCP headers, permit POST/GET/DELETE, disable response buffering, and allow requests lasting several minutes for initial synchronization. Port 8080 should remain private.

## Connect accounts

1. Visit `https://todo.example.com/admin` and enter `TODO_ADMIN_TOKEN` into the password field. It stays in page memory and is cleared on reload.
2. Select **Add Microsoft account**, open the displayed Microsoft verification URL in another tab, and enter the user code. Sign in and consent to the requested To Do permissions.
3. Select **Check sign-in** after completing Microsoft sign-in. The server enforces Microsoft's polling interval.
4. Select **Synchronize**. Wait until state `ready`, then inspect last successful sync time.
5. Repeat for a second account if needed. Select **Set default** to change which account is used by reads that omit `account_id`.

**Disconnect and erase local data** deletes the selected account's refresh token, cache, search index, synchronization progress and operation history. Microsoft tasks stay unchanged. A request already submitted before disconnect may finish remotely. To revoke the upstream app grant as well, use Microsoft's account permissions page as described in [authentication](authentication.md).

## Connect ChatGPT or another MCP client

Add `https://todo.example.com/mcp` as a remote MCP server. Use OAuth and the client registration in your authorization provider. Register the exact callback URI shown by the client, use Authorization Code with PKCE S256, and request the resource audience `https://todo.example.com/mcp` plus scope `todo:mcp`.

The service publishes protected resource metadata at:

- `/.well-known/oauth-protected-resource/mcp`
- `/.well-known/oauth-protected-resource`

An unauthorized MCP request returns 401 with a `WWW-Authenticate` discovery hint. Issuer discovery, authorization, token refresh and client registration are provided by your OAuth provider. Only the configured owner subject can use this instance. Static development tokens are restricted to loopback and are intended for local probes.

## First-use acceptance

Use a dedicated test list and disposable tasks, approving the intended write operations before running them.

1. Confirm an unauthenticated request receives 401 and a valid owner login works. Test natural OAuth token expiration and refresh.
2. Call `list_accounts`, then `refresh`, `list_lists` and `search_tasks` with a Chinese title/body query.
3. Create a task with reminder and recurrence using a unique operation key. Read it back and inspect Microsoft To Do on your phone.
4. Add/update/check/delete a step, add/update/delete a link, complete and reopen a task. Confirm the next occurrence of a repeating task follows Microsoft's behavior.
5. Change/create/delete a disposable task in Microsoft's app. Wait for background synchronization and verify the cached results and timestamps.
6. Add a second account and verify explicit scope isolation. Test default switching and account removal.
7. Restart the service. Check authorization, cached data and continued delta synchronization.

The repository's synthetic automated acceptance is tracked in [validation](validation.md). Record the actual client version and Microsoft account type when performing these live checks.

## Backup, upgrade and recovery

The `todo-data` volume contains a SQLite WAL database. Back it up together with the encryption key, storing the key separately from data backups. Task bodies are plaintext in the database; restrict volume access and encrypt VPS backups/disks as appropriate.

For a simple consistent backup, stop the service, copy the **whole volume**, then restart. Do not copy just `todo.db` while WAL writes are active. Test restore into an isolated instance before depending on the backup.

Before an upgrade, back up the data and `.env`, record the image/commit, rebuild and restart. Keep the previous image for rollback. Do not run two instances sharing the same SQLite volume. Startup changes interrupted pending writes to `unknown`; use the original operation key to inspect/reconcile them. Database schema versions newer than the running binary are rejected.

Expired delta cursors trigger a replacement scan. The previous visible cache remains readable until the replacement stream is atomically published. A failed initial scan returns incomplete freshness state. If Microsoft reports `reauthorization_required`, reconnect the account in the management page.

## Operational limits

- Up to 32 Microsoft accounts, one authorized owner, one SQLite-backed server process.
- 256 KiB MCP request limit, 1 KiB serialized JSON-RPC ID limit, 1 MiB complete tool response budget.
- Up to 100 cached tasks per page; narrow the query when bodies exceed the response budget.
- 4 MiB maximum Graph response per page; collection and synchronization page budgets prevent runaway upstream loops.
- 32 simultaneous admitted tool calls; busy clients retry with the original operation key.
- Background interval defaults to 60 seconds. Safe reads retry bounded 429/5xx failures; mutations are never automatically replayed.
- Logs contain status/error codes rather than tokens or task bodies. Avoid verbose dependency tracing in production.
