# Inbox listings: paging, change-based long polls, admission limits (Phase 9b)

`GET /v1/inbox` used to load a mailbox's whole history on every call, and the apps' long poll
returned at once whenever the mailbox held unread mail. Under load that meant every poll read every
row, and the postbox kept accepting work until it held 5.6 GB (`docs/ops/2026-10-28-load-tests.md`).
This page describes what changed and what clients may rely on.

## What every existing client keeps

Nothing a shipped client sends changes meaning, and nothing it reads disappears:

- **The web inbox, iOS/Mac, Android, Windows and Linux** list `include_sent=true&include_read=true`
  and adopt the response whole, treating a missing message as deleted. Their listing is the same
  rows, in the same order, with the same fields, as before. The server now reads them through an
  index instead of scanning the history, and the test `pages_list_exactly_what_the_whole_history_listed`
  compares the new listing with the old one for every combination of `include_sent`,
  `include_read`, `filtered`, `thread_id`, `actions` and `fold`.
- **The only bound is a ceiling** of `POSTBOX_INBOX_MAX_ROWS` rows per response (default 5,000).
  Production's largest mailbox is far below it (see the Phase 9b ledger entry for the figure at deploy
  time), so no shipped client sees a shorter listing today. A mailbox above the ceiling gets its
  newest 5,000 rows and `page.has_more: true`. Raise the ceiling before any mailbox approaches it, or
  move the apps to paging first: an app that adopts a capped listing whole would hide older history.
- **CLI `inbox` / `watch`, agentd, the hosted MCP and ChatGPT tools** read unread mail. Those
  listings now read only unread rows (partial index `messages_unread_by_mailbox`) and the rows they
  refer to.
- New fields are additive: `page`, `version`, and `seq` on each message. Every client ignores
  unknown fields (checked: web, Swift `Codable`, Kotlin `ignoreUnknownKeys`, `System.Text.Json`,
  Python, the CLI's `serde_json::Value`).

## Long polls (`wait`)

| Listing | `wait` ends when |
|---|---|
| unread (`include_read` absent) | there is unread mail — unchanged; an agent draining its inbox wants exactly this |
| history (`include_read=true`), or any request with `since` | the listing **changed** since the one this client was last given (or since `since`) |

Before, a history long poll also ended "when there is unread mail". The iOS, Android and Linux apps
poll `include_read=true&wait=25` back to back, so while one message sat unread each of them re-listed
the whole history in a tight loop. Now the wait ends on a change: new mail in or out, a read on
another device, a delete, an edit, a receipt, an expiry, mail moved out of Filtered.

"Last given" is remembered per mailbox and per credential (a digest of the `Authorization` header,
never the header itself), so each signed-in device has its own baseline. A client the server has not
answered yet, or one it forgot (a restart, the 100,000-entry table), gets an answer at once — never a
missed change. Clients may also send the `version` from their last response as `since`.

## Paging (opt-in)

| Parameter | Meaning |
|---|---|
| `limit` | most rows on the page (1 … ceiling) |
| `after=<seq>` | rows stored after that row, oldest first — "what is new since my last page" |
| `before=<seq>` | rows older than that row, newest first — "load older" |

`seq` is the row's storage order and is never reused (`store::events`). Without a cursor an unread
listing returns its oldest rows (a queue to drain) and a history listing its newest. The response's
`page` says `limit`, `has_more`, `oldest_seq` and `newest_seq`; a page may hold fewer rows than
`limit` when some expired. Folded views list an original whose edit is on the page, even if the
original itself is older, as the full listing always did, so pages can overlap by such originals;
merge by `message_id`.

A paging client still has to learn about deletions. Today they reach clients only as absence from a
full listing; a client that switches to `after` deltas needs a `message.deleted` event first. That is
why no shipped client has been moved to paging yet.

## Admission control

`overload.rs`, outermost on the public router:

| Class | Routes | Limit |
|---|---|---|
| stream | `GET /v1/events`, `GET /v1/shell/agent/events` | `POSTBOX_MAX_STREAMS` (20,000) open; each ends after `POSTBOX_STREAM_MAX_SECS` (4 h ± 10 %) and resumes (`Last-Event-ID`); 15 s keep-alives unchanged; per-account cap unchanged |
| long poll | `GET /v1/inbox?wait=N` | `POSTBOX_MAX_LONG_POLLS` (20,000) parked; each limited to `N` + the request timeout |
| ordinary | everything else | `POSTBOX_MAX_INFLIGHT` (256) running; wait up to `POSTBOX_QUEUE_WAIT_MS` (250) for a slot; `POSTBOX_REQUEST_TIMEOUT_SECS` (30) |
| exempt | `GET /health` | — |

Inbox listings additionally share `POSTBOX_MAX_LISTINGS` (32) slots, which with the row ceiling
bounds the memory listings can hold. Refusals are `503` with `Retry-After` and
`{"error":"overloaded"|"request_timeout"}`; `/metrics` counts them
(`pigeonpost_requests_shed_total`, `pigeonpost_requests_timed_out_total`).

Request bodies by class: 64 KiB on GET/HEAD/DELETE/OPTIONS; 512 KiB on ordinary JSON routes; 2 MiB
(axum's old default) on `/v1/send`, `/mcp`, group posts and group resync; uploads keep their own
limits. A direct message's text is limited to 64 KiB by the envelope anyway.

The production container also has a memory limit (Phase 9b ledger entry) so that a bug becomes a
restart rather than a host under swap.
