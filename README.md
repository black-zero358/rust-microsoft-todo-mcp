# Rust Microsoft To Do MCP

A self-hosted Rust MCP server for Microsoft To Do, designed for a personal VPS and one or more Microsoft accounts. **M1 implementation candidate:** Streamable HTTP, separate client and Microsoft authorization, persistent cache, Chinese search, background delta sync, and task/list/checklist/link management. Attachments are planned for M2.

[Deployment](docs/deployment.md) · [Authentication](docs/authentication.md) · [Architecture](docs/architecture.md) · [Validation](docs/validation.md) · [M1 agreement](docs/M1-SPEC.md)

## Quick start

You need Docker Compose, a public HTTPS domain, a Microsoft application registration, and an OAuth provider that issues RS256 access tokens. The MCP service validates the owner's subject, issuer, resource audience and scope on every request. Your Microsoft accounts are authorized separately using device sign-in.

1. Clone this repository and copy `.env.example` to `.env`.
2. Follow [authentication setup](docs/authentication.md) to configure your Microsoft Client ID and the MCP OAuth provider. Generate separate administrator and encryption secrets. Keep `.env` private.
3. Run `docker compose up -d --build`. The container exposes port 8080 on the VPS loopback interface only.
4. Put HTTPS in front using the [Caddy example](deploy/Caddyfile).
5. Open `https://todo.example.com/admin`, enter your administrator token, add a Microsoft account, and synchronize it.
6. Connect the MCP client to `https://todo.example.com/mcp` with OAuth. Start with `list_accounts`, `list_lists`, and `search_tasks`.

See [deployment and acceptance](docs/deployment.md) for the complete steps and live Microsoft/ChatGPT checks.

## Tools

| Area | Tools |
| --- | --- |
| Accounts and cache | `list_accounts`, `sync_status`, `refresh`, `search_tasks` |
| Lists | `list_lists`, `get_list`, `create_list`, `update_list`, `delete_list` |
| Tasks | `list_tasks`, `get_task`, `create_task`, `update_task`, `delete_task`, `complete_task`, `reopen_task` |
| Steps | `list_steps`, `create_step`, `update_step`, `delete_step` |
| Links | `list_linked_resources`, `create_linked_resource`, `update_linked_resource`, `delete_linked_resource` |
| Write recovery | `get_operation`, `reconcile_operation` |

Task input covers title, body, importance, categories, status, due/start dates, reminders and recurrence. Omit a field to preserve it; send `null` for supported nullable fields to clear it. Dates use Graph's `dateTime` and `timeZone` objects. Plain-text bodies are escaped and sent as HTML for Graph compatibility. See [Graph contracts](docs/graph-contract.md).

Reads use the default account unless you specify one. Writes always require an explicit `account_id` and a new `operation_key` for each intended change. Retry the same intent with the **same key**. An uncertain write is stored as `unknown`; inspect and reconcile it before doing anything else. Creates with uncertain outcomes return matching candidates for review and are never automatically replayed.

Search reads the local cache, including Chinese title and body text, and supports account/list/status/due-date filters. Results include freshness and pagination. `get_task`, `get_list`, steps and links read Graph directly. `refresh` brings the selected account's cache up to date.

## Development

The toolchain is pinned to Rust 1.99.1. Tests use synthetic local HTTP services and temporary SQLite databases.

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --locked
cargo build --release --locked
```

Production credential setup, connecting real Microsoft accounts and deployment remain operator actions. See [the validation ledger](docs/validation.md) for the exact checks performed on this implementation.

## License

This project is licensed under the **GNU Affero General Public License, version 3 only** (`AGPL-3.0-only`). See [LICENSE](LICENSE) for the full license text. The project does not grant an “or any later version” option.

Third-party dependencies retain their own licenses and notices; see [third-party notices](THIRD_PARTY_NOTICES.md). Design lessons were drawn from [rust-email-mcp](https://github.com/black-zero358/rust-email-mcp); this implementation uses Graph-specific transport, synchronization and operation contracts.
