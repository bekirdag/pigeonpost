# Web desktop parity — 2026-09-17

## Objective
Bring applicable native desktop usability improvements to the browser inbox and publish the verified web update. Native app builds, store submissions and backend changes are outside this task.

## Audit and scope
- Already present: focused Settings pages, cross-platform account handles, safe Markdown, attachments/drop, compose-first-message, subjects, archiving and sender permissions.
- Missing: clear /owner labels for default addresses, stable mailbox ordering and removal of duplicate address/copy bar; automatic leading slash on new addresses.
- Missing reliability: loading/retry feedback and mailbox-scoped asynchronous state. Old inbox/contact/subject/archive requests and mutations must not replace a newly selected mailbox.
- Missing conversation tools: find with highlighted matches and navigation, confirmed deletion of a subject, resizable desktop columns.
- Message history: render newest ten initially; load older history on request/upward scroll; preserve the reading anchor on refresh and prepend; follow bottom only while the reader remains there; respond to delayed image/layout sizing; find must include older messages.
- Preserve keyboard/touch behavior, light/dark contrast, inert message content, attachment/send attribution and browser responsiveness.

## Dependency order
1. Isolate branch; inspect current native/web code and existing tests. Record AST and impact/DAG evidence.
2. Address helpers and mailbox request context; reset transient state and cancellation on switching/sign-out.
3. Stable paged renderer, search, subject actions and column controls; update HTML/CSS together.
4. Behavioral regression tests (out-of-order requests, selected mailbox send/ack, history/find/delete, settings, address paste, layout).
5. Run all web tests, staged Docdex gate and browser checks at desktop/phone widths including delayed media, refresh while reading, keyboard and dark appearance.
6. Review/commit/push changes and release via repository workflow. Back up only the affected production inbox root; deploy static assets, check hashes and public health, record rollback location and ensure scoped repositories are clean/synced.

## Evidence and limitations
- Main baseline 5f884745189d14528121b4bd6a620cedc5a7ade9, clean/synced.
- Docdex JS symbols found; impact graph has no import edges because this is a classic-script static app. Explicitly inspect HTML script order and test harness evaluation. DAG export mcp-32 available but sparse/old trace nodes; do not infer coverage from empty graphs.
- Docdex clone directive twice returned database locked; profile retrieval/save works and the new web scope preference persisted. Proceed using explicit user authorization and read skill/profile requirements.
- Docdex local qwen draft for input normalization was invalid (deleted selected text/returned without updating input); primary will implement and verify.
- /v1/inbox currently returns a full snapshot. Latest-ten paging bounds DOM/layout work, not API payload size; no new backend pagination contract is invented.
