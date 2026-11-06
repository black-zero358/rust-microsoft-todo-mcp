# Microsoft Graph contract

Verified against the Microsoft Graph v1.0 documentation on 2026-11-06. The service uses delegated `Tasks.ReadWrite`, `User.Read` and `offline_access`. Graph remains authoritative for task state and account permissions.

## Endpoints and data

All paths below are relative to `https://graph.microsoft.com/v1.0`.

| Resource | Collection | Item operations |
| --- | --- | --- |
| Lists | `/me/todo/lists` | GET, POST; GET/PATCH/DELETE `/{listId}` |
| Tasks | `/me/todo/lists/{listId}/tasks` | GET, POST; GET/PATCH/DELETE `/{taskId}` |
| Steps | `/me/todo/lists/{listId}/tasks/{taskId}/checklistItems` | GET, POST; GET/PATCH/DELETE `/{stepId}` |
| Linked resources | `/me/todo/lists/{listId}/tasks/{taskId}/linkedResources` | GET, POST; GET/PATCH/DELETE `/{resourceId}` |
| List changes | `/me/todo/lists/delta` | GET |
| Task changes | `/me/todo/lists/{listId}/tasks/delta` | GET |

Identifiers are percent-encoded as individual path segments. Pagination and delta URLs are retained whole, including the opaque query tokens. Every URL is validated before a token is obtained: the production origin is exactly HTTPS `graph.microsoft.com` on its default port, the path stays under `/v1.0/`, and credentials, fragments and redirects are rejected.

The task schema supports title, body, importance, categories, status, dates, reminders and patterned recurrence. Completion uses `status: completed`; reopening uses `status: notStarted`. Completion of a recurring task triggers a refresh from Graph rather than manufacturing a follow-up task locally. Explicit `null` values for nullable task fields pass through to Graph; omitted properties remain omitted.

`itemBody` defines both `text` and `html`. The To Do task-update page currently describes its body as HTML-only, while the general task-resource page links the generic `itemBody` type. The deployment acceptance checklist must exercise both requested body forms and clearing behavior on the target account. Sending escaped text as HTML is the conservative interoperability option.

Lists identify built-in lists using `wellknownListName`: `defaultList` and `flaggedEmails`; user-created lists use `none`. The tool layer rejects renaming/deleting built-in lists and leaves other Microsoft permissions to Graph.

A linked resource has `applicationName`, `displayName`, `externalId` and `webUrl`. The resource definition and update endpoint establish this model; the update page's request-body table only lists `id`, so it is unsuitable as a complete writable-field schema. The implementation excludes server-assigned IDs and timestamps from user mutation input.

## Transport and operation outcomes

- The client has a 10-second connection timeout and 45-second request timeout. Responses and request bodies are limited to 4 MiB.
- Safe reads get at most four attempts for transport failures, interrupted bodies, HTTP 429 and HTTP 500/502/503/504. Backoff starts at 250 ms. `Retry-After` seconds or HTTP date are honored up to 30 seconds; a longer delay returns throttling/unavailability instead of retrying too early.
- Mutations are sent once. Transport interruption, malformed or oversized success responses, HTTP 408 and server errors return `write_unknown`. The operation ledger owns reconciliation and any later retry decision.
- A complete HTTP 204 is a successful mutation with a null resource body.
- HTTP 400/422 map to `invalid_argument`, 401 to `account_auth_required`, 403 to `graph_forbidden`, 404 to `not_found`, 409/412 to `graph_conflict`, 410 to `sync_cursor_expired`, and 429 to `graph_throttled`.
- A 401 asks the user to reconnect the Microsoft account. Normal expiration is handled before Graph requests by the account token-refresh path. The transport does not repeatedly replay rejected requests.
- Raw upstream error bodies, task contents, authorization headers and cursor tokens are excluded from transport error messages.
- The convenience collection reader is bounded to 1,000 pages, 100,000 items and 32 MiB of collected JSON, and detects pagination cycles. Delta sync uses individual pages and durable cursors.

## Automated checks

Inline Graph tests exercise the actual reqwest path against synthetic localhost HTTP endpoints: bounded pagination, hostile next links, strict URL validation, retry exhaustion, 429 recovery, redirect refusal, mutation non-replay, 204, 401/404/400 classification, oversized responses and malformed mutation responses. The test-only constructor is absent from production builds and accepts only a `127.0.0.1` HTTP origin.

## Real-account acceptance

Before promoting a deployment for daily use, complete the M1 scenarios with a dedicated test list:

1. Connect a personal account and, when available, a work/school account. Confirm account isolation and token refresh across a restart.
2. Create and update text/HTML bodies, Unicode titles/categories, timezone-bearing dates and reminders. Clear supported nullable fields, disable reminders and remove recurrence.
3. Complete a recurring task and compare the next occurrence, dates, steps and cached results with the Microsoft client.
4. Exercise list/step/linked-resource CRUD and built-in list restrictions.
5. Make changes in the Microsoft client and verify list/task delta, deletions and cached Chinese search.
6. Connect the target MCP client over HTTPS with its actual OAuth provider and verify its access-token refresh.

## Sources

- [Task delta](https://learn.microsoft.com/en-us/graph/api/todotask-delta?view=graph-rest-1.0)
- [List delta](https://learn.microsoft.com/en-us/graph/api/todotasklist-delta?view=graph-rest-1.0)
- [Update task](https://learn.microsoft.com/en-us/graph/api/todotask-update?view=graph-rest-1.0)
- [Task resource](https://learn.microsoft.com/en-us/graph/api/resources/todotask?view=graph-rest-1.0)
- [Item body](https://learn.microsoft.com/en-us/graph/api/resources/itembody?view=graph-rest-1.0)
- [List resource](https://learn.microsoft.com/en-us/graph/api/resources/todotasklist?view=graph-rest-1.0)
- [Update checklist item](https://learn.microsoft.com/en-us/graph/api/checklistitem-update?view=graph-rest-1.0)
- [Linked resource model](https://learn.microsoft.com/en-us/graph/api/resources/linkedresource?view=graph-rest-1.0)
- [Update linked resource](https://learn.microsoft.com/en-us/graph/api/linkedresource-update?view=graph-rest-1.0)
