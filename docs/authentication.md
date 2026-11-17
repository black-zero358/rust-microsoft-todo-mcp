# Authentication and account authorization

The service has two independent authorization boundaries:

1. **MCP client → this service:** an external OAuth provider issues a signed access token for this MCP resource. The service checks the signature, issuer, audience, expiration, owner subject and scope on every request.
2. **This service → Microsoft Graph:** each connected Microsoft account grants delegated To Do access through Microsoft's device authorization flow. The service encrypts refresh tokens before storing them in SQLite.

The management interface uses a separate administrator token. Never reuse a Microsoft access token, OAuth client secret or MCP access token as that administrator token.

## Register the Microsoft application

1. Open the [Microsoft Entra admin center](https://entra.microsoft.com/) and go to **Identity → Applications → App registrations → New registration**.
2. Choose **Accounts in any organizational directory and personal Microsoft accounts** if the instance will connect both personal and work/school accounts. Choose an organization-specific type only when intentionally limiting the instance to that organization.
3. Copy the **Application (client) ID** into `MICROSOFT_CLIENT_ID`.
4. Under **Authentication → Advanced settings**, enable **Allow public client flows**. This service uses the device-code flow as a public client, so it needs no Microsoft client secret or redirect URI.
5. Under **API permissions**, configure Microsoft Graph **delegated** permissions `User.Read` and `Tasks.ReadWrite`. The authorization request also asks for `offline_access` so the service can refresh access in the background. Organizational tenant policies may require an administrator to grant consent or permit device-code authentication.
6. Set `MICROSOFT_TENANT=common` for personal and organizational accounts. `consumers`, `organizations`, or a specific tenant UUID can deliberately restrict the sign-in authority.
7. In the service's management page, start an account login. Open the displayed Microsoft verification address, enter the displayed user code and check the application name and permissions before consenting. Keep the page open while it polls. After connection, the account appears in the account list and can be selected as the default.

Requested permission scope is account-wide To Do read/write access. Microsoft presents the consent screen. Only authorize accounts you intend this single-owner service to manage.

Device login sessions expire after Microsoft's advertised lifetime, with a maximum of 30 minutes. The service enforces the polling interval and `slow_down` responses, stores device codes only in memory, and exposes only the user code and verification address to the management page. A restart cancels incomplete logins. Disconnecting an account cancels pending logins to prevent a late authorization result from reconnecting it.

Microsoft references:

- [Device authorization grant](https://learn.microsoft.com/en-us/entra/identity-platform/v2-oauth2-device-code)
- [Microsoft Graph To Do overview](https://learn.microsoft.com/en-us/graph/api/resources/todo-overview?view=graph-rest-1.0)
- [Microsoft Graph permissions reference](https://learn.microsoft.com/en-us/graph/permissions-reference)

## Set up OAuth for remote MCP clients

Bring an OAuth/OIDC authorization server that supports:

- Authorization Code flow with **PKCE S256**, and refresh tokens for the selected MCP client.
- An HTTPS issuer and standard authorization-server or OpenID Connect discovery metadata.
- A JWKS endpoint on that same HTTPS origin.
- **RS256 JWT access tokens**, containing `iss`, `aud`, `exp`, `sub` and a space-delimited `scope` or `scp` claim. Opaque access tokens are not supported.
- A resource/audience exactly equal to `TODO_PUBLIC_URL` plus `/mcp`.
- The configured scope, `todo:mcp` by default.

Use a dedicated single-owner OAuth identity. Pin its exact `sub` claim in `TODO_OAUTH_OWNER_SUBJECT`. The subject is the provider's stable internal subject identifier, not necessarily the email address or username. Tokens belonging to other users are rejected, including other users of the same provider.

Register the MCP client at that provider. For ChatGPT, use the exact redirect URL shown by ChatGPT, select the provider's public-client/PKCE configuration when supported, and enter the registered OAuth client ID in the client connection settings. If the client requires confidential-client authentication, configure that at the OAuth provider and client. Such a client secret belongs to the provider/client pair and is not a Microsoft application secret.

The OAuth provider must issue the configured **resource audience** in access tokens. An ID token, a token whose audience is only the OAuth client ID, or a Graph access token will fail this service's audience/scope checks. Verify the provider's resource/audience mapping before attempting a remote connection.

The service publishes protected-resource metadata at its advertised well-known route and returns a Bearer authentication challenge for unauthenticated MCP requests. It is an OAuth protected resource, not an authorization server: login screens, consent, client registration, refresh-token policy and client credentials are managed by the external provider.

### Concrete Rauthy recipe

If you already run the Rauthy deployment used by `rust-email-mcp`, reuse that issuer and create a **separate** `todo-mcp` client. For a fresh provider installation, the reference project's [pinned Rauthy deployment guide](https://github.com/black-zero358/rust-email-mcp/blob/1f1a0b6d52ea2a92b8007758b5b55cb306766849/docs/deployment.md#配置认证) and [Compose template](https://github.com/black-zero358/rust-email-mcp/blob/1f1a0b6d52ea2a92b8007758b5b55cb306766849/deploy/compose.yaml) describe its service/bootstrap setup. Keep its provider database and secrets separate from To Do data.

In Rauthy's administrator UI, create `todo:mcp` as a scope and configure this client:

| Field | Value |
| --- | --- |
| Client ID | `todo-mcp` |
| Client type / token authentication | Public / `none` |
| Grant flows | `authorization_code`, `refresh_token` |
| PKCE | S256 required |
| Redirect URI | Exact callback URI displayed by your MCP client |
| Allowed/default scopes | `openid`, `todo:mcp` |
| Access-token algorithm / lifetime | RS256 / 300 seconds |
| Allowed resources / default audience | `https://todo.example.com/mcp` |

The reference Rauthy 0.37.2 setup uses client fields `allowed_resources` and `default_aud` as arrays, both containing that resource URL. Set them through the UI or the authenticated client-management API while preserving all other fields. Do not put management API keys in this repository or application config.

Set `TODO_OAUTH_ISSUER` to the issuer from Rauthy's discovery document and `TODO_OAUTH_JWKS_URL` to its `jwks_uri`; for the reference layout these are `https://auth.example.com/auth/v1` and `https://auth.example.com/auth/v1/oidc/certs`. Set `TODO_OAUTH_OWNER_SUBJECT` to your dedicated user's stable ID.

Before connecting, inspect discovery: the public token-authentication methods must advertise `none`, and PKCE must advertise S256. The reference version's discovery omitted `none`; its pinned deployment guide documents an explicit discovery-file override at the reverse proxy. Use that override only if the deployed version has this exact omission, and regenerate it after issuer/provider changes.

This recipe is adapted from the reference deployment. For this To Do candidate, provider deployment and real ChatGPT login remain live acceptance steps: verify issuer discovery, callback, PKCE, exact access-token resource audience, owner subject, scope and natural refresh. Do not loosen audience or owner checks to work around a configuration mismatch.

### Environment variables

| Variable | Meaning |
| --- | --- |
| `TODO_PUBLIC_URL` | Public origin, for example `https://todo.example.com`. No path, query or fragment. |
| `TODO_OAUTH_ISSUER` | Exact HTTPS issuer appearing in access tokens; preserve its path and trailing slash if the provider uses them. |
| `TODO_OAUTH_JWKS_URL` | HTTPS signing-key URL on the issuer's origin. |
| `TODO_OAUTH_AUDIENCE` | Exactly `https://todo.example.com/mcp` for the example origin. |
| `TODO_OAUTH_SCOPE` | One required scope, default `todo:mcp`. |
| `TODO_OAUTH_OWNER_SUBJECT` | Exact subject identifier of the allowed owner. |
| `TODO_ADMIN_TOKEN` | At least 32 random characters, dedicated to the management API/page. |
| `TODO_ENCRYPTION_KEY` | 32 random bytes encoded as 64 hexadecimal characters. Preserve it across restarts. |
| `MICROSOFT_CLIENT_ID` | Microsoft public application's client UUID. |
| `MICROSOFT_TENANT` | Default `common`; also accepts `consumers`, `organizations`, or a tenant UUID. |
| `TODO_BIND` | IP address and port, default `127.0.0.1:8080`; Docker commonly uses `0.0.0.0:8080` behind a private proxy network. |
| `TODO_DATA_DIR` | Persistent data directory, default `data`. |
| `TODO_SYNC_INTERVAL_SECS` | Background synchronization interval, 15–86400 seconds, default 60. |

Generate independent values, for example with `openssl rand -hex 32`. Keep the administrator token and encryption key in private deployment secrets, never in source control, URLs, screenshots or logs. Back up the encryption key separately from the database; losing it requires reconnecting accounts. Changing the configured key does not migrate existing credentials.

### Local development only

`TODO_DEV_MCP_TOKEN` enables a static Bearer token in place of OAuth. It requires at least 32 characters, must differ from the administrator token, and is accepted only when both the bind address and public URL are loopback. This mode is for local protocol tests. Remote ChatGPT connections use the OAuth configuration above.

## Disconnect, revoke and rotate

**Disconnecting an account in this service** deletes its local credentials, cached data, search data and cursors, removes cached access tokens and stops future authorized background requests. Database generation checks prevent stale synchronization or refresh work from recreating local account state. A request already sent to Microsoft may finish there.

**Revoking the Microsoft application grant** is a separate provider action. Remove the application's permissions through the relevant account's Microsoft consent/application management page, then disconnect it locally:

- Personal accounts: [Manage application permissions](https://account.live.com/consent/Manage).
- Work/school accounts: [My Apps](https://myapplications.microsoft.com/) or your organization's administrator, depending on tenant policy.

Revocation is intentionally not implemented as `revokeSignInSessions`, which would affect unrelated Microsoft sessions. If Microsoft invalidates a refresh token, the service reports that the account must be reauthorized.

Microsoft can rotate refresh tokens. The service encrypts and saves a replacement token before publishing the refreshed access token. Updates are conditional on the current account generation; a removed or reauthorized account cannot be overwritten by an older refresh.

Changing `TODO_ADMIN_TOKEN` requires a service restart and invalidates the old management token. Change OAuth owner subject only when transferring access intentionally. Rotate OAuth signing keys at the provider; the service refreshes its bounded JWKS cache and validates every access token.

## Security and deployment checks

- Expose only HTTPS publicly. Keep the application port private behind the reverse proxy and configure the proxy to preserve the public Host and Origin behavior expected by the server.
- Treat the administrator token as full control over every connected account. Restrict management access further with a private network or proxy policy when practical.
- The instance has one owner. Every accepted MCP owner token can access the accounts connected to that instance. This is not a multi-user tenant isolation service.
- Refresh tokens are encrypted with AES-256-GCM and bound to the account ID. Task bodies, names and search data are stored locally as ordinary SQLite data. Encrypt the host disk or volume if those contents need at-rest encryption.
- Access tokens and device codes stay in memory. Error messages omit upstream authorization bodies and secrets. Disable proxy request-header logging and keep tracing at the documented normal level.
- Verify unauthenticated MCP access fails, an unrelated user's token fails, and a legitimate owner's access token works. Then test normal OAuth token expiry/refresh and reconnect after a service restart.
- Finally connect a disposable Microsoft account and exercise real Graph CRUD/synchronization before granting access to daily-use accounts. The automated suite uses synthetic credentials and mocks; tenant policy and end-client behavior are checked in this deployment step.
