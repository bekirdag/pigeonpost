# Pigeonpost inbox

A messenger for the hosted postbox: conversations on the left, the thread on the right, one pane at
a time on a phone. Static HTML, CSS and JavaScript, with no build step, runtime dependencies or framework.

Intended home: `inbox.pigeonpost.dev`.

```
index.html   the shell
app.css      the theme — the site's tokens (site/style.css), messenger layout
app.js       auth, the postbox API, the thread model, rendering
config.js    endpoints and the OIDC client. No secret
```

## What a conversation is made of

Both halves come from the postbox. A delivered message is sealed to the recipient; a sent one is
sealed a second time to the sender and stored in the sender's own mailbox, so a thread reads the
same on every device and includes messages sent from the CLI or over MCP.

`GET /v1/inbox?include_sent=true&include_read=true` returns the conversation. **Without that flag the endpoint is
unchanged** — received mail only — because every other caller reads it as "mail addressed to me",
and an agent draining its inbox must not find its own replies in there and treat them as somebody's
request.

Each message carries `direction` (`in` / `out`) and `peer` — the other end of the conversation,
whichever way it went — so the client groups threads without inferring anything from which fields
happen to be set. A sent copy carries no autonomy verdict at all: your own words were never subject
to an admission decision, and a plausible-looking `review` on them would be a lie.

Pending sends, per-conversation drafts, staged files and reading positions stay in memory.
Authentication, the selected mailbox and appearance preferences persist in `localStorage`.

## Your agents

A namespace owner's mailboxes — `/bekir/su_iam`, `/bekir/docdex` — are the people they most want to
write to, so they are pinned at the top of the list under **Your agents**, above every conversation,
when they have corresponded with this mailbox. Silent mailboxes stay in the identity picker. The
rest of the list is everyone who has written in or been added as a contact.

There is no entitlement check here, and there should not be one. An account holds the mailboxes it
holds: a free account has one anonymous mailbox and this group comes out empty on its own, while a
namespace owner sees their fleet. `/v1/identities` is already scoped to the authenticated account,
so asking the browser to decide who is paid would only add a second, weaker answer to a question the
server has settled.

Two different things are one click apart, so the app is explicit about which one you are doing:

- **Writing to an agent** — pick it from the list. The thread shows what it has sent *this* mailbox
  and what you have sent it. The header says `· your mailbox`.
- **Reading as an agent** — the identity switcher, or **Open this mailbox** in the sender panel.
  Now you are that agent, seeing the mail it received.

Messages between your own mailboxes are not stranger-throttled: `tier_of` gives any account-owned
mailbox the `Account` tier, which sits above the throttle line, so a fleet can talk to itself
freely.

## Deploying

Static files. Serve the directory; there is nothing to build. Two things must be configured
elsewhere first, and the app is inert without either.

**1. The realm must know this origin.** The app uses the existing `pigeonpost-web` public client
with PKCE, exchanging the code with the realm directly from the browser. On that client:

- redirect URI — `https://inbox.pigeonpost.dev/` (the exact URI the app sends, not a wildcard)
- web origin — `https://inbox.pigeonpost.dev`

Without the web origin the browser blocks the token exchange, and a missing redirect URI fails at
the realm with `invalid_redirect_uri`. Both surface as a sign-in that returns to a signed-out page,
so check them first when that happens. Append to the existing values — the account page's own
redirect URI lives on the same client.

**2. The postbox must allow this origin.** `CORS_ORIGINS` in `crates/pigeonpost-postbox/src/main.rs`
lists it as of this change, but the running container is whatever was last deployed — the origin
takes effect only after the postbox is rebuilt and redeployed. Until then every API call fails CORS
and the app loads to an empty shell.

The same change adds `PUT` to `Access-Control-Allow-Methods`, which was missing: `/v1/contacts`,
`/v1/policy` and `/v1/workspace` are `PUT` routes and were unreachable from any browser.

Verify both in one shot, from the browser console on the deployed origin:

```js
await fetch("https://postbox.pigeonpost.dev/v1/whoami", { headers: { authorization: "Bearer " + localStorage.ppi_token } })
```

## Running it locally

`file://` will not work — the OIDC redirect and the API both need a real origin. Serve the
directory and point a redirect URI at wherever you serve it:

```
python3 -m http.server 8080 --directory site-inbox
```

Then add `http://localhost:8080/*` to the client's redirect URIs and web origins, and add the same
origin to `CORS_ORIGINS` on a locally run postbox. Do not add localhost to the production realm.

## Tests

```
npm ci --prefix site-inbox --ignore-scripts
npm test --prefix site-inbox
```

The `inbox web` CI job runs the same suite on every pull request and main push.
The harness drives the real `index.html` and `app.js` in jsdom against fixture data
copied from the postbox's response shapes, and asserts the parts that are quiet when they break —
thread assembly and ordering, a scoped request rendering as a request, opening a thread
acknowledging its unread mail, a send being addressed and attributed correctly, and a hostile
message body staying inert text.

`test/onboarding.test.mjs` also covers a signed-in account with no mailbox, creation failures,
expired sessions, and retries after a successful mint whose response or following listing was
lost. Visibility assertions include ancestors: the original signup bug unhid **Create my inbox**
inside a panel that `render()` immediately hid because the user had a token.

Sign-in and mailbox readiness are separate. Setup remains visible until a mailbox is loaded;
errors offer a retry. Before creating on a retry, the page checks for an existing mailbox so a
lost response does not mint another. Existing account holders open their current mailbox directly.

## Decisions worth knowing

**Displaying a page of a thread acknowledges that page’s received mail.** That is what a messenger does, but here `ack` is not
only a read receipt: it is also how an agent sharing the mailbox learns a message has been dealt
with. A human reading ahead in this app marks mail handled for everyone on that mailbox.

**Bodies are inserted with `textContent`, never as markup**, and nothing in a body is ever acted on.
A message is another agent's text. The server decides what may be acted on and says so in
`autonomy`; this app only ever shows that decision.

**A scoped request renders as a request** — verb, arguments, note, and either `auto` or `held` with
the reason spelled out — rather than as the raw JSON envelope it is on the wire.

**Threads key on the handle** when the sender has one, because that is what trust matches on and
what a person recognises, falling back to the key address. Outbound is normalised through the same
map, so writing to `/k/…` and hearing back from `/bekir/agent1` stays one conversation.

**A file drop is taken from the browser everywhere in the window.** Dropping a file on the
conversation attaches it, which is what people try before they find the paperclip. The reason the
handlers are on the document rather than on the composer is the other half: a drop the page does
not handle is one the browser handles, and its answer is to navigate the tab to the file — throwing
away a half-written message and the thread it was being written in. So every file drop is
cancelled, one over an open conversation stages the file, and one with no conversation open says so
instead. Drags of text and links are left alone, because rearranging words inside the message box
is a drag too. A dropped folder is refused where it can be explained, rather than at upload time
where it fails as unreadable bytes.

**Sign-in is `offline_access`.** A mail app that signs you out every half hour is one nobody opens.
The refresh token lives in `localStorage` and is dropped on sign out.

## Remaining backend limitations

- **Unread counts across the fleet.** An agent row shows unread only for mail that agent sent *this*
  mailbox. Whether `/bekir/docdex` has unread mail of its own is not visible until you open it,
  because that would mean polling every mailbox on every cycle.
- **Pagination.** `/v1/inbox` returns the whole mailbox on every poll, so a large mailbox is a large
  response. Fine at current volumes, not free forever.


## Desktop parity (September 2026)

The browser inbox has focused Settings pages, account-wide handles, mailbox address copying in
Settings and the picker, and short default-inbox labels such as `/bekir` and `/alp`. The main
readable mailbox leads the picker; other named roots and their children follow, with raw keys last.
New conversation addresses gain their leading slash automatically and require a first message.

Mailbox changes cancel old requests and discard late responses, including A→B→A switches. Loading
and retry status stay visible without blocking the picker. Drafts and staged files stay with their
mailbox, peer and subject in memory; signing out clears them. Uploads, sends and read acknowledgements
use the mailbox captured when the operation started.

A conversation initially renders its latest ten messages. Scrolling upward or selecting **Load
earlier messages** expands history while preserving a message anchor. Refreshes reuse unchanged
bubbles and keep the reading position; only a reader at the bottom follows new arrivals or layout
changes. **Latest messages** returns to the newest page. The existing postbox API still supplies a
full snapshot: this is display paging, not server pagination.

**Find in conversation** (Ctrl/Cmd+F while a conversation is open) searches the full loaded history,
shows highlighted matches and previous/next navigation, and renders a small area around an older
match. Escape closes the find bar or current dialog before leaving the conversation. A selected
thread can be deleted after confirmation; deletion affects only this mailbox's copy.

On wider desktop screens, column dividers can be dragged or focused and adjusted with Left/Right
(Shift for a larger step). Double-click restores the default width. Widths persist on this browser;
phone and narrow desktop layouts adapt independently. File attachments remain authenticated downloads.

Run `npm test --prefix site-inbox` from the repository root. `test/desktop-parity.test.mjs` covers
mailbox races, page boundaries, older-history search, draft/upload ownership, deletion, input/IME,
keyboard navigation and retry. `test/fixtures.mjs` contains only synthetic wire-shaped data and can
also back a local browser acceptance server; neither file is deployed. Browser validation must
check real scrolling, delayed layout, resizing and responsive/dark layouts because jsdom cannot
measure their geometry.
