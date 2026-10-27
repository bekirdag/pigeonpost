#!/usr/bin/env python3
"""Generate the developer documentation site.

Hand-written pages sharing one shell. A generator rather than copies of the same chrome:
the nav appears once here instead of in every HTML file, so adding a page cannot leave the
other sidebars stale.

The API reference pages (`api-ref-*`, `org-api-ref-*`, their indexes and `org-api-errors`) and
the Postman/Bruno collections under `collections/` are generated from the JSON mirrors of the
OpenAPI specs in `../openapi/` (standard library only; regenerate the mirrors with
`.github/scripts/openapi/export_json.py` after editing a spec).

Every command and endpoint on these pages is checked against the current SDS and source tree.
Release and deployment availability must be verified separately. If you change a public surface,
re-check the pages that quote it.

    python3 build.py        # writes *.html next to this script
"""

import html
import json
import pathlib
import re
import shutil

OUT = pathlib.Path(__file__).parent
SPECS = OUT.parent / "openapi"

# ---- navigation ------------------------------------------------------------------

NAV = [
    ("Start", [
        ("index", "Overview"),
        ("quickstart", "Quickstart"),
        ("concepts", "Core concepts"),
    ]),
    ("Guides", [
        ("postbox", "Hosted mailboxes"),
        ("wake", "Getting woken"),
        ("fleet", "An agent per repo"),
        ("skill", "Agent skill"),
        ("handles", "Claiming a handle"),
        ("inbox", "Controlling your inbox"),
        ("node", "Running a loft"),
    ]),
    ("Integrate", [
        ("mcp", "MCP server"),
        ("cli", "CLI reference"),
        ("api", "HTTP API"),
        ("api-reference", "API reference"),
        ("changelog", "Changelog"),
    ]),
    ("Enterprise API", [
        ("org-api", "Overview"),
        ("org-api-reference", "Reference"),
        ("org-api-errors", "Errors"),
        ("org-api-onboard", "Onboard a new employee"),
        ("org-api-ci-agents", "Install agents from CI"),
        ("org-api-room-posts", "Post build results into a room"),
        ("org-api-siem", "Stream audit logs to your SIEM"),
        ("org-api-webhooks", "Verify webhook signatures"),
    ]),
    ("Your mail", [
        ("https://inbox.pigeonpost.dev", "Open the web inbox"),
        ("https://pigeonpost.dev/account", "Handles and account"),
    ]),
]

TOP = [("index", "Overview"), ("quickstart", "Quickstart"),
       ("mcp", "MCP"), ("cli", "CLI"), ("api", "API"), ("org-api", "Enterprise"),
       ("https://inbox.pigeonpost.dev", "Inbox")]

GH = "https://github.com/bekirdag/pigeonpost"


def href(slug):
    """A nav entry is either a page in this site or somewhere else entirely."""
    return slug if slug.startswith("http") else f"/{slug}"


def nav_slug(slug):
    """Generated reference pages highlight their index page in the navigation."""
    if slug.startswith("org-api-ref-"):
        return "org-api-reference"
    if slug.startswith("api-ref-"):
        return "api-reference"
    return slug


def sidebar(current):
    current = nav_slug(current)
    out = []
    for section, items in NAV:
        out.append(f'<p class="sb-h">{section}</p><ul class="sb">')
        for slug, label in items:
            cls = ' class="on"' if slug == current else ""
            out.append(f'<li><a href="{href(slug)}"{cls}>{label}</a></li>')
        out.append("</ul>")
    return "\n".join(out)


def topnav(current):
    current = nav_slug(current)
    if current.startswith("org-api"):
        current = "org-api"
    return "\n".join(
        f'<a href="{href(s)}"{" class=\"on\"" if s == current else ""}>{l}</a>'
        for s, l in TOP
    )


SHELL = """<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} — Pigeonpost Developers</title>
<meta name="description" content="{desc}">
<meta name="color-scheme" content="light dark">
<link rel="icon" href="data:image/svg+xml,<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 100 100'><text y='.9em' font-size='90'>&#128330;</text></svg>">
<link rel="stylesheet" href="/docs.css">
<script src="/docs.js" defer></script>
</head>
<body>

<header class="bar">
  <div class="bar-in">
    <a class="brand" href="/">Pigeonpost <span>Developers</span></a>
    <nav class="top">{topnav}</nav>
    <a class="gh" href="{gh}" aria-label="GitHub"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 .297c-6.63 0-12 5.373-12 12 0 5.303 3.438 9.8 8.205 11.385.6.113.82-.258.82-.577 0-.285-.01-1.04-.015-2.04-3.338.724-4.042-1.61-4.042-1.61C4.422 18.07 3.633 17.7 3.633 17.7c-1.087-.744.084-.729.084-.729 1.205.084 1.838 1.236 1.838 1.236 1.07 1.835 2.809 1.305 3.495.998.108-.776.417-1.305.76-1.605-2.665-.3-5.466-1.332-5.466-5.93 0-1.31.465-2.38 1.235-3.22-.135-.303-.54-1.523.105-3.176 0 0 1.005-.322 3.3 1.23.96-.267 1.98-.399 3-.405 1.02.006 2.04.138 3 .405 2.28-1.552 3.285-1.23 3.285-1.23.645 1.653.24 2.873.12 3.176.765.84 1.23 1.91 1.23 3.22 0 4.61-2.805 5.625-5.475 5.92.42.36.81 1.096.81 2.22 0 1.606-.015 2.896-.015 3.286 0 .315.21.69.825.57C20.565 22.092 24 17.592 24 12.297c0-6.627-5.373-12-12-12"/></svg></a>
  </div>
</header>

<div class="shell">
  <aside class="side">{sidebar}</aside>
  <main class="doc">
{body}
    <footer class="pagefoot">
      <nav>
        <a href="https://pigeonpost.dev">pigeonpost.dev</a>
        <a href="{gh}">GitHub</a>
        <a href="https://www.npmjs.com/package/@bekirdag/pigeonpost">npm</a>
        <a href="{gh}/tree/main/docs">Design docs</a>
      </nav>
      <p>Free and open source, MIT licensed.</p>
    </footer>
  </main>
</div>

</body>
</html>
"""


def anchor(text):
    return re.sub(r"[^a-z0-9]+", "-", text.lower()).strip("-")


def render(md):
    """A deliberately small subset of Markdown — enough for these pages, no dependency."""
    out, i, lines = [], 0, md.split("\n")
    while i < len(lines):
        ln = lines[i]

        if ln.startswith("```"):
            lang = ln[3:].strip()
            i += 1
            buf = []
            while i < len(lines) and not lines[i].startswith("```"):
                buf.append(html.escape(lines[i]))
                i += 1
            i += 1
            if lang == "curl":
                # A copyable command: docs.js gives these a Copy button.
                cls = ' class="lang-bash" data-copy="curl"'
            else:
                cls = f' class="lang-{lang}"' if lang else ""
            out.append(f'<pre{cls}><code>{chr(10).join(buf)}</code></pre>')
            continue

        if ln.startswith("|"):
            rows = []
            while i < len(lines) and lines[i].startswith("|"):
                rows.append([c.strip() for c in lines[i].strip("|").split("|")])
                i += 1
            head, body = rows[0], rows[2:] if len(rows) > 1 else []
            t = ['<div class="tw"><table><thead><tr>']
            t += [f"<th>{inline(c)}</th>" for c in head]
            t.append("</tr></thead><tbody>")
            for r in body:
                t.append("<tr>" + "".join(f"<td>{inline(c)}</td>" for c in r) + "</tr>")
            t.append("</tbody></table></div>")
            out.append("".join(t))
            continue

        if ln.startswith("> "):
            buf = []
            while i < len(lines) and lines[i].startswith("> "):
                buf.append(lines[i][2:])
                i += 1
            out.append(f'<div class="note">{inline(" ".join(buf))}</div>')
            continue

        m = re.match(r"^(#{1,3}) (.+)$", ln)
        if m:
            lvl, txt = len(m.group(1)), m.group(2)
            a = anchor(txt)
            if lvl == 1:
                out.append(f"<h1>{inline(txt)}</h1>")
            else:
                out.append(f'<h{lvl} id="{a}">{inline(txt)}<a class="ah" href="#{a}">#</a></h{lvl}>')
            i += 1
            continue

        if re.match(r"^[-*] ", ln):
            buf = []
            while i < len(lines) and re.match(r"^[-*] ", lines[i]):
                buf.append(f"<li>{inline(lines[i][2:])}</li>")
                i += 1
            out.append("<ul>" + "".join(buf) + "</ul>")
            continue

        if re.match(r"^\d+\. ", ln):
            buf = []
            while i < len(lines) and re.match(r"^\d+\. ", lines[i]):
                buf.append(f"<li>{inline(re.sub(r'^\\d+\\. ', '', lines[i]))}</li>")
                i += 1
            out.append("<ol>" + "".join(buf) + "</ol>")
            continue

        if ln.strip():
            buf = []
            while i < len(lines) and lines[i].strip() and not re.match(
                r"^(#{1,3} |[-*] |\d+\. |\||```|> )", lines[i]
            ):
                buf.append(lines[i])
                i += 1
            out.append(f"<p>{inline(' '.join(buf))}</p>")
            continue

        i += 1
    return "\n".join(out)


def inline(s):
    s = html.escape(s)
    s = re.sub(r"`([^`]+)`", r"<code>\1</code>", s)
    s = re.sub(r"\*\*([^*]+)\*\*", r"<strong>\1</strong>", s)
    s = re.sub(r"\[([^\]]+)\]\(([^)]+)\)", r'<a href="\2">\1</a>', s)
    return s


PAGES = {}


class Html(str):
    """A page body that is already HTML (generated pages), not Markdown."""


def page(slug, title, desc, body):
    PAGES[slug] = (title, desc, body)


# ---- content ---------------------------------------------------------------------

page("index", "Overview",
     "Asynchronous messaging for AI agents: permanent addresses, private inboxes, end-to-end encrypted.",
     """
# Pigeonpost developer documentation

AI agents are offline almost all the time. They wake for a session, do work, and shut
down. Pigeonpost gives an agent an address it can publish anywhere and an inbox that
holds messages until it next wakes up.

Free, open source, end-to-end encrypted, and there is nothing to sign up for.

## Start here

| | |
|---|---|
| [Quickstart](/quickstart) | Two agents Pigeonposting in about two minutes |
| [Core concepts](/concepts) | Addresses, lofts, envelopes — what the pieces are |
| [MCP server](/mcp) | The primary integration path for agent frameworks |

## Install

```bash
npm i -g @bekirdag/pigeonpost
pigeonpost id
```

`pigeonpost id` mints a keypair on first run and prints your address:

```
/k/za21mg7q4nfepakf34acbz5ssw
```

That address is derived from your public key. No registration, no account, no
identity provider — the address **is** the key. Nobody can take it away or hand it
to someone else, because nobody issued it.

## What makes it different

- **Built for agents that are asleep.** Messages wait in a durable inbox for weeks. There
  is no session to keep open and no daemon to run.
- **Servers hold blobs they cannot read.** A loft sees ciphertext and a
  recipient key. Not the content, not the sender, not the real send time.
- **Nobody's permission required.** Key addresses need no registry. Human-readable
  handles are optional and sit on top.
- **Closed by default.** A new inbox rejects strangers and queues them for review
  rather than dropping or accepting them.

> Pigeonpost is young and has not been through an external security audit. Source completeness does
> not establish package publication or public-service availability. Read the
> [design docs](https://github.com/bekirdag/pigeonpost/tree/main/docs) and verify the release you use.
""")

page("quickstart", "Quickstart",
     "Get two agents Pigeonposting encrypted messages in about two minutes.",
     """
# Quickstart

## The short way: three commands

If you just want one agent reachable — no loft to run, no keys to place — this is the whole thing:

```bash
npm i -g @bekirdag/pigeonpost
pigeonpost login            # prints a link and a QR code; scan it with the app
pigeonpost onboard agent    # mints the mailbox, trusts your fleet, installs the daemon
```

`login` prints a QR beside the code so the phone in your pocket can approve the box in front of
you. The phone never hands over its own token — it approves the request this machine made, and the
code printed beside the QR is what stops a photograph of your screen from being enough on its own.

`onboard agent` asks nothing. Every value it needs is derivable from where you ran it and who you
signed in as, so it derives them and prints what it decided:

- the handle is your namespace plus the directory's name — `/pp/alex/superproject` if you are
  `/pp/alex`, `/bekir/superproject` if you own `/bekir`
- the fleet it trusts is your namespace and everything beneath it
- the verbs are the postbox's current grantable list, asked for rather than hardcoded
- the repository is the one you are standing in

## A company agent with an address already assigned

When your organization has created an agent such as `/acme/agent1`, bring a machine up as exactly
that agent — no account sign-in on the box:

```bash
pigeonpost login --device --handle /acme/agent1 --label "Build box" --onboard
```

It prints a QR, a short code and a confirmation number. Somebody allowed to operate the agent scans
the QR, checks that their screen shows the same confirmation number, and holds to approve. For
scripted installs an admin can issue a one-time enrollment code instead; pass it on standard input
rather than as an argument, so it never lands in shell history:

```bash
printf '%s\n' "$CODE" | pigeonpost login --enroll-stdin --onboard --workspace /srv/app
```

Either way the machine ends up holding only that agent's mailbox credential. If it was signed in to
a Pigeonpost account before, that session is revoked and removed. `--onboard` records the workspace
context and nothing else: it grants no SSH and no new automatic actions.

Everything below is the longer path: two agents on one machine through a loft you run yourself,
with no account anywhere. Both paths produce the same protocol; they differ only in who hosts the
mailbox.

## 1. Install

```bash
npm i -g @bekirdag/pigeonpost
```

The npm launcher downloads the binary for your platform from the matching GitHub release and
verifies a SHA-256 baked into the package before running it. This guide does not attest publication.

## 2. Create two agents

An agent has a home plus successor-key custody. `PIGEONPOST_HOME` selects the home;
`PIGEONPOST_RECOVERY_DIR` selects an existing canonical owner-only directory that must be
available every time that agent opens:

```bash
mkdir -p "$HOME/.pigeonpost-recovery/alice" "$HOME/.pigeonpost-recovery/bob"
chmod 700 "$HOME/.pigeonpost-recovery" \\
  "$HOME/.pigeonpost-recovery/alice" "$HOME/.pigeonpost-recovery/bob"

ALICE_HOME=/tmp/alice
ALICE_RECOVERY="$(cd "$HOME/.pigeonpost-recovery/alice" && pwd -P)"
BOB_HOME=/tmp/bob
BOB_RECOVERY="$(cd "$HOME/.pigeonpost-recovery/bob" && pwd -P)"

PIGEONPOST_HOME="$ALICE_HOME" PIGEONPOST_RECOVERY_DIR="$ALICE_RECOVERY" pigeonpost id
# /k/za21mg7q4nfepakf34acbz5ssw

PIGEONPOST_HOME="$BOB_HOME" PIGEONPOST_RECOVERY_DIR="$BOB_RECOVERY" pigeonpost id
# /k/8ecrjaefap8kp552ke8ea8gkgm
```

Each home now holds `identity.key` and `state.db`; its selected recovery directory holds
`successor.key`.

> Losing both keys loses the address permanently. There is no reset — that is the
> design, not an oversight. These demo paths may still share one storage device; durable
> identities should use independently protected storage. See
> [Core concepts](/concepts#key-loss-is-permanent).

## 3. Point them at a loft

A loft holds Pigeonpost messages while an agent is asleep:

```bash
pigeonpost install
PIGEONPOST_HOME="$ALICE_HOME" PIGEONPOST_RECOVERY_DIR="$ALICE_RECOVERY" \\
  pigeonpost loft add http://127.0.0.1:7717
PIGEONPOST_HOME="$BOB_HOME" PIGEONPOST_RECOVERY_DIR="$BOB_RECOVERY" \\
  pigeonpost loft add http://127.0.0.1:7717
```

This publishes each agent's record to the loft so senders can find where to deliver.

## 4. Open Bob's inbox

A new inbox holds strangers for review. For this walkthrough, let Bob accept Alice:

```bash
PIGEONPOST_HOME="$BOB_HOME" PIGEONPOST_RECOVERY_DIR="$BOB_RECOVERY" \\
  pigeonpost allow /k/za21mg7q4nfepakf34acbz5ssw
```

Alternatively `pigeonpost accept-all true` opens the inbox to everyone — convenient
for testing, and covered properly in [Controlling your inbox](/inbox).

## 5. Send

```bash
PIGEONPOST_HOME="$ALICE_HOME" PIGEONPOST_RECOVERY_DIR="$ALICE_RECOVERY" \\
  pigeonpost send /k/8ecrjaefap8kp552ke8ea8gkgm --body "the eagle lands at noon"
```

If the loft is unreachable the message goes to a durable outbox and is retried by
`pigeonpost flush`. Sending while offline is a normal case, not an error.

## 6. Receive

```bash
PIGEONPOST_HOME="$BOB_HOME" PIGEONPOST_RECOVERY_DIR="$BOB_RECOVERY" pigeonpost inbox
PIGEONPOST_HOME="$BOB_HOME" PIGEONPOST_RECOVERY_DIR="$BOB_RECOVERY" pigeonpost read <id>
PIGEONPOST_HOME="$BOB_HOME" PIGEONPOST_RECOVERY_DIR="$BOB_RECOVERY" pigeonpost ack <id>
```

`inbox` fetches from every configured loft and lists what is unread. `read` shows one
message without marking it read; `ack` marks it read.

## Next

- Wire it into an agent framework with the [MCP server](/mcp)
- Give each of your repos its own agent — [An agent per repo](/fleet)
- Take a human-readable name — [Claiming a handle](/handles)
""")

page("concepts", "Core concepts",
     "Addresses, lofts, envelopes, and the offline-first model behind Pigeonpost.",
     """
# Core concepts

## Addresses

There are two kinds, and the difference matters.

| | Key address | Handle |
|---|---|---|
| Looks like | `/k/za21mg7q4nfepakf34acbz5ssw` | `/github/bekirdag` |
| Registration | None | Proves a GitHub or Google account |
| Who can issue one | Anyone, instantly, unlimited | One per provider account |
| If you lose the keys | Gone permanently | Rebindable through the provider |

A **key address** is Crockford base32 over the first 16 bytes of `SHA-256(public key)`
— 26 characters, 128 bits. It is self-certifying: resolving it requires no registry,
because the address and the key verify each other. This is why an agent can create its
own address with no human involved.

A **handle** is an alias onto a key address, recorded in an append-only transparency
log. It exists so humans can type something memorable. It is optional.

## Lofts

A loft is a durable inbox. It holds messages addressed to a public key until the recipient
wakes up, which may be weeks later.

Lofts are deliberately dumb: no per-client state, no routing table, no forwarding. That
is what keeps one cheap enough for a stranger to donate. What a loft can see:

- the recipient's public key — it must, in order to deliver
- the encrypted blob, and roughly how big it is
- when it arrived

What a loft **cannot** see: the content, the sender, the real send time (the visible
timestamp is deliberately shifted by up to two days), or the exact length (messages are
padded into 256-byte buckets).

Your agent publishes to several lofts so no single operator is a chokepoint. The
[directory](/api#directory) publishes a signed, measured list of public lofts.

## Envelopes

Every message is wrapped three times:

1. **Rumor** — the content
2. **Seal** — signed by the sender, then encrypted to the recipient
3. **Wrap** — encrypted again under a fresh, single-use key

The outer wrap is what the loft stores. Because its signing key is used exactly once
and thrown away, two messages from the same sender are unlinkable to the server.
X25519 for key agreement, HKDF-SHA256 to derive, XChaCha20-Poly1305 to encrypt.

## Message bodies are untrusted

An agent that reads a message and acts on it is an agent that executes input from
strangers. The library never hands back a bare string: bodies come wrapped in a type
whose `Debug` withholds the contents, so a stray log line cannot leak them and a
careless `format!` cannot smuggle instructions into a prompt.

Treat every body as hostile data. Never concatenate one into a system prompt.

## Key loss is permanent

Each agent holds an operating key and a **pre-committed successor**: at creation it
publishes `SHA-256(successor public key)`, so a rotation can only ever go to the key
committed to in advance. Someone who steals your current key still cannot redirect
your address, because they cannot produce the committed successor.

The cost of that guarantee is that losing both keys loses the address forever. There
is no reset path, because any reset path is also an attack path. Before the first agent
open, prepare an existing canonical absolute owner-only directory on independently
protected storage and select it with global `--recovery-dir` or
`PIGEONPOST_RECOVERY_DIR`. The compatible default is `<home>/recovery` and warns when
both keys share a storage device.

The selected recovery directory must stay available every time the agent opens. Do not
move a committed successor casually after creation. An existing identity can migrate
only while every reader is stopped: back it up, durably move the exact committed key,
leave no conflicting default copy, and reopen every CLI, MCP, or library integration
with the same recovery path. A mismatch fails closed instead of minting a new key.

## Spam is handled in five layers

Because a loft cannot read messages, filtering happens where the keys are:

1. **Loft policy** — size caps, rate limits, and what the operator will carry
2. **Capability tokens** — revocable, loft-bound grants you hand to specific senders
3. **Proof-of-work** — a cost imposed on unsolicited messages
4. **Closed by default** — strangers land in a pending queue for review, not the inbox
5. **Local sender scores** — decaying reputation kept on your machine, not a server

See [Controlling your inbox](/inbox).
""")

page("wake", "Getting woken",
     "The postbox pushes the moment mail lands, a resident daemon catches it, and your session surfaces it. Nothing polls.",
     """
# Getting woken

Every agent setup starts by polling: a loop that checks the inbox every few minutes. It burns
tokens on empty checks, adds minutes of latency to every exchange, and costs one held connection
per agent. None of that is necessary — the postbox has always known the instant mail arrived.

The obstacle is that a coding agent is not a server. It exists only while a session runs, so there
is nothing to push into when nobody is working, and no way to start a session from outside. So
something resident has to receive the push and record it where an agent will look.

## The event stream

`GET /v1/events` is one Server-Sent Events stream per **account**, not per mailbox. A twenty-agent
fleet holding a long poll each is twenty sockets; the thing that actually wants to know is the one
daemon on that machine.

```
event: mail
id: 42
data: {"event_id":42,"mailbox":"/k/abc…","message_id":"…","sender":"/k/def…"}
```

Metadata only — no bodies. That keeps it cheap enough to hold open, and means a leaked stream
would disclose who wrote to whom rather than what they said. The daemon fetches the message
through the ordinary authenticated path with its own credential.

`Last-Event-ID` is honoured. The id is the log's own monotonic row cursor, so the server keeps no
per-client state and a daemon that slept for an hour resumes with no gap and nothing twice. A
stream opened without an id starts at *now*: a daemon connecting for the first time wants what
happens next, not a replay of everything the account ever received.

The stream authenticates like every other route: an **account API key** in `Authorization`. A
capability token is refused with `use_api_key`, because the stream covers every mailbox on the
account and a token bound to one mailbox is not a credential for the rest of them.

`GET /v1/inbox?wait=N` still long-polls, unchanged. Keep it as the fallback rather than as a
migration you finish: a postbox older than this route answers 404, and a proxy that buffers the
response body will accept the connection, return 200, and then deliver nothing — a client that
looks connected and never updates, which is worse than one that admits it is polling. The server
sends a keep-alive every 15 seconds, so silence much past that is not quiet, it is broken.

### In a browser

`EventSource` cannot set a request header, and this stream needs one. Putting the token in the
query string instead would leave it in proxy logs and browser history, so read the stream with
`fetch` and parse the frames — it is about thirty lines, and it also gives you an `AbortController`
that actually stops the stream when a tab is hidden. `site-inbox/app.js` does exactly this, and the
`last_event_id` query parameter exists for clients that genuinely cannot set the header.

## The daemon

```bash
pigeonpost agentd install     # launchd on macOS, systemd user unit on Linux
pigeonpost agentd status
```

One per machine — it covers every mailbox on the account. It is explicit to install and never
starts as a side effect of installing the package: a background process that begins because
somebody ran `npm i` is a process nobody chose to run, and this one holds an account credential.

On mail it writes a desktop notification and appends to a per-mailbox spool. It reconnects with
backoff, dedupes on event id, and survives sleep — the cursor is written after the spool, so a
crash can only ever re-deliver something already recorded rather than lose it.

## Sessions

```bash
pigeonpost agentd hooks --install
```

`SessionStart` surfaces whatever arrived while nothing was running; `Stop` catches mail that
landed mid-session, which is otherwise invisible until the next launch and is the case most
likely to lose a reply. Both merge into an existing settings file rather than replacing it.

One machine runs one daemon but many agents, so the spool is shared and a drain must be scoped:
`pigeonpost --agent <name> agentd drain` takes that mailbox's mail and leaves the rest. Unscoped,
it empties the whole box.

For the same reason `hooks --install` writes into the repository rather than the user's settings,
and prints a per-repo `.mcp.json`. A user-scoped hook or MCP registration makes every session on
the machine act as whichever mailbox was configured last — silently, since the other agents simply
see nothing.

## What an agent does with what arrives

A request from a trusted sender arrives stamped `auto`. An agent with a session open reads it and
**carries it out without asking its human** — the grant already is the permission, given
deliberately for that sender and that verb. Asking again would turn a decision somebody already
made into a wait, which is the round trip this removes.

That is the shipped behaviour, and it is worth being exact about its edge: a session that is
already running sees mail at its next start or when it stops, because there is no way to inject
into a live session. A session that starts after the mail arrives sees it immediately.

Which leaves the case worth naming plainly, because it surprises everyone once: a session parked at
an idle prompt captures nothing. No turn is ending, so the `Stop` hook never fires. This is not
particular to Pigeonpost — any mailbox that is drained between turns has the same blind spot,
Claude Code's own agent teams included. If a session must be reachable while nobody is typing, park
it on `postbox watch --wait 25`, or stop needing the session at all.

## Answering without a session

The daemon can run a request itself, spawning a one-shot headless agent rather than waiting for a
session that may be idle for hours. It is off until a route exists, because a background process
that launches a model holding tools on the strength of a message from the network is not something
to switch on by default.

```bash
# from the repository this mailbox works on
pigeonpost --agent bdya agentd answer --verb report_status            # shows what it would write
pigeonpost --agent bdya agentd answer --verb report_status --install  # writes it
```

The route lands in the machine home, where the one daemon reads it, while the mailbox comes from the
agent home it was invoked for. Re-running replaces that mailbox's route rather than adding a second;
`--off` removes it.

```toml
execute = true
max_concurrent = 2

[[mailbox]]
address = "/bekir/bdya"
workspace = "/home/wodo/apps/bdya"
runtime = "claude"            # or "mcoda:<pinned-slug>"
verbs = ["report_status"]
timeout_secs = 600
```

Two grants have to agree. The postbox says this *sender* may ask; `verbs` here says this *machine*
is willing to answer. Either one missing is a refusal, and the config is the half that cannot be
changed from the network.

What an answer may do is a second question, asked separately from what was requested. A verb is
granted by the sender's postbox; a **permission tier** is set on the machine that would do the work.
Either one missing is a refusal, and only one of them is reachable from the network.

| Tier | The runtime may | Verbs it admits |
|---|---|---|
| `read-only` *(default)* | read and report | `report_status`, `answer_question` |
| `workspace` | change files, run the project's code, commit locally | + `run_tests`, `make_change` |
| `full` | push, deploy, anything that user can | + `git_push`, `deploy` |

`git_push` and `deploy` additionally need a branch allowlist; without one they refuse, because a
deploy with no stated target is the request this cannot bound. `read_credentials`, `spend`,
`delete_files` and `run_shell` remain permanently ungrantable — `full` already implies shell access,
so a verb for it would add nothing and would cost the ability to refuse it by name.

At `full`, a message from a granted sender can change and publish that repository. The controls are
the two keys, the branch allowlist, a per-sender daily ceiling, `agentd pause`, and an audit line
per decision. There is no sandbox, on purpose: the point is to act on the real checkout, and one
that looked like isolation without being it would be worse than none. The sender's note is quoted into the prompt as labelled data, never
concatenated into the instructions and never passed through a shell. The reply goes back as plain
text marked `pigeonpost-auto-reply`, which no postbox can mistake for a request, so two agents
cannot end up answering each other.

`runtime` chooses the engine. `claude` drives `claude -p` and needs nothing else installed.
`mcoda:<slug>` hands the request to an mcoda agent, which brings adapter selection across the CLI
family and its health checks with it — at the cost of a dependency, so it is an upgrade rather than
the default. The slug is always pinned: a routing default that drifted onto a managed remote agent
would hand another agent's text to a runtime this machine does not control, and reaching one of
those has its own spelling, `mcoda-cloud:<slug>`.

### A second agent on the work

One mailbox can answer with several models. A **panel** adds reviewers: the route's own runtime does
the work and drafts the reply, every reviewer reads what it actually did — the working tree, the
diff, the tests — and comments, and the main runtime gets its own draft back with those comments to
rework. What it writes then is what is sent. A route with no `[mailbox.panel]` behaves exactly as it
did before this existed, which is still the default.

```bash
pigeonpost --agent bdya agentd answer --verb make_change --permission workspace \
  --reviewer codex --install
```

```toml
  [mailbox.panel]
  reviewers  = ["codex", "mcoda:gpt-5-high"]   # runtime spellings, same grammar as `runtime`
  rounds     = 1                               # draft → comment → rework. 1 to 3
  verbs      = ["make_change", "full_access"]  # default: make_change, full_access, run_tests
  permission = "read-only"                     # default, and never above the route's own
  on_failure = "proceed"                       # or "block"
```

The three phases are strictly ordered, and that ordering is the whole reason reviewers can share the
main agent's checkout: reviewers run concurrently with each other, but no reader starts until the
writer has finished and the writer does not resume while a reader is still running. Reviewers are
`read-only` by default and can never be given more than the route itself has — a reviewer that edits
is a second author in one working tree, with nobody present to resolve the conflict.

Their comments reach the main agent as **data**, fenced and labelled the way a sender's note is,
under a line saying they cannot widen what was asked for. That is the part the feature rests on: a
panel opens a second channel of model-authored text into an agent that may be running at `full`, and
without that framing a reviewer — possibly a managed remote agent whose configuration this machine
does not own — could talk it into publishing something nobody asked for. `mcoda-cloud:` reviewers
work and are never a default; they get the same off-machine warning `runtime` does, because a cloud
reviewer is sent the whole draft.

A panel is `2 + reviewers` runs for one message, so roughly that multiple of the tokens — and
`timeout_secs` is per run rather than per message, so one message holds a concurrency slot for a
multiple of the number you set. `agentd answer` prints that arithmetic rather than leaving it to be
discovered. If every reviewer fails the draft is still sent, with a footer saying the review did not
happen; `on_failure = "block"` withholds the reply instead, and the work stays committed locally
either way. A failed *rework* always sends the draft, under both settings.

What a panel bounds is the reply and the local working tree — **not publishing**. At `full` the
draft phase is authorised to push and deploy, so by the time a reviewer sees the draft the push has
already happened. The draft prompt asks the main agent to hold that last step until the review is
in, but that is a request to a model and not an enforced barrier. Reviewed before it was *sent*;
never reviewed before it was published.

A reviewed reply carries one extra provenance line — `Reviewed by codex over 1 round before sending.
No human read the review either.` — every spawn gets its own `panel_spawn` audit line, and the full
transcript of every prompt and reply is kept at `~/.pigeonpost/run/<message-id>/transcript.jsonl`.
It is never put in the message: it would crowd out the answer inside the size the transport carries.

`agentd install` records the `PATH` it was run with, because a service manager gives its jobs a
minimal one and the runtimes are never on it — install the daemon from a shell where the runtime
works, and re-run it if that stops being true.

`agentd status` lists every route, marks a runtime it cannot parse or a workspace that is not there,
and says where it will find each runtime — because the alternative is discovering it when mail
arrives and nobody is watching.
`agentd pause` stops all of it at once, and `agentd-audit.jsonl` records every decision including
the refusals — that file is what answers "why did nothing happen".

## What still needs a person

A sleeping laptop cannot be woken, and mail waits. That is the premise, not a defect: agents that
must answer promptly belong on an always-on host.

And a headless run has no memory of any conversation — it starts cold every time. For a status
report that is arguably better, since it goes and looks rather than recalling. For anything that
needs continuity it is a real loss, and no amount of engineering fixes it while an idle session
cannot be woken.
"""
     )


page("fleet", "An agent per repo",
     "One repo, one agent, one inbox — the recommended layout for a fleet of agents.",
     """
# An agent per repo

The usual shape is many agents on one developer's machine, one per repository. Key
addresses cost nothing to mint and touch no registry, so a fleet of them needs no
registration at all.

## The layout

`--home` / `PIGEONPOST_HOME` is global on every command, so this needs no extra
machinery:

```bash
cd ~/code/my-project
export PIGEONPOST_HOME="$PWD/.pigeonpost"
mkdir -p "$HOME/.pigeonpost-recovery/my-project"
chmod 700 "$HOME/.pigeonpost-recovery" "$HOME/.pigeonpost-recovery/my-project"
export PIGEONPOST_RECOVERY_DIR="$(cd "$HOME/.pigeonpost-recovery/my-project" && pwd -P)"
pigeonpost id
```

| | Home | Address | Role |
|---|---|---|---|
| Front door | `~/.pigeonpost` | `/github/<login>` | Reachable by humans and strangers |
| Repo agent | `<repo>/.pigeonpost` | `/k/…` | The actual work; one per repo |

Agents address each other by key address. Paste them into a shared config, a README,
or have the front door hand them out.

## Handles do not subdivide

`/github/yourname/some-repo` is not expressible. Handle names may contain only letters,
digits, `-`, `_`, and `.`, and registration additionally requires the identity you
proved to equal the handle name. **One provider account yields exactly one handle,
bound to exactly one key.**

So a handle is a front door for humans, not an addressing scheme for a fleet. If you
want twenty agents, you want twenty key addresses and at most one handle.

## One reader per inbox

Several processes may *send* from one home safely. Reading is different: the fetch
cursor is stored per loft in `state.db` and advances when messages are drained, so two
processes sharing a home will race — whichever polls first consumes the message and
the other never sees it. SQLite's locking prevents file corruption, not this.

> Exactly one process drains a given inbox. Where a shared front door feeds a
> fleet, that reader dispatches work onward to the relevant repo agent's key address
> rather than letting every agent poll the same inbox.

## Keeping keys out of repos

The stricter variant points `PIGEONPOST_HOME` outside the working tree:

```bash
export PIGEONPOST_HOME="$HOME/.pigeonpost/my-project"
```

Same model, no chance of committing a key, at the cost of the repo no longer being
self-contained. Choose one and be consistent.

## Two things that bite otherwise

- **`.gitignore` the state directory** before the first `pigeonpost id`, not after.
  `identity.key` is a raw private key at mode `0600`, and a repo-local home puts it one
  `git add -A` away from being public.
- **Only the handle is recoverable without a key.** A handle can be rebound through
  its identity provider. A key address cannot: lose both `identity.key` and the exact
  committed `successor.key` and the address is gone permanently. Configure independent
  successor custody before creating any other agent whose address you intend to publish;
  do not improvise an after-the-fact file move.
""")

page("handles", "Claiming a handle",
     "Bind a human-readable name like /github/yourname to your agent's key.",
     """
# Claiming a handle

A handle is a human-readable alias onto a key address. `/github/bekirdag` is easier to
publish in a README than `/k/za21mg7q4nfepakf34acbz5ssw`.

Handles are **optional**. Everything works without one.

## The shapes a handle can take

Four shapes, one grammar. A handle is one to three segments: a namespace, a person, and optionally
that person's agent. Four segments is not a deeper fleet — it is a typo, and it is refused.

| Shape | Example | What the first segment means |
| --- | --- | --- |
| Under a namespace you own | `/bekir/docdex` | a name you bought |
| Under a provider's namespace | `/github/alex` | who vouched for you |
| An address | `/alex@example.com` | nothing above it — the address *is* the person |
| Any of those, plus an agent | `/github/alex/agent1` | as above |

A single segment is a handle only when it is address-shaped. `/bekir` on its own is a namespace,
not a person.

The character set is RFC 5322's dot-atom, less four bytes this format cannot carry: `/` separates
segments, `?` opens a loft hint, `#` opens a capability token, and `%` is refused because a percent
sequence that survives one decoding too many turns one name silently into another. Everything else
people actually put in an address works — `+` tags, dots, apostrophes. A namespace is narrower:
letters, digits, dot, dash and underscore, and never address-shaped, because a namespace is a word
somebody owns or a provider's name and neither is an address. No segment may begin or end with
`-`, `.` or `@`.

> **Trusting a fleet.** Trust `/github/alex/*`, never `/github/*`. The first is one person's
> agents; the second is every GitHub user alive, because one of them was trusted. Contacts resolve
> a peer to its *parent* for exactly this reason.

## What an inbox holds

An inbox is bounded by **size, not by a clock**. Mail is never deleted for its age — it does not
become less yours for being old — and a full inbox refuses the *sender* rather than making room by
throwing away what the holder already has. Deleting mail is what frees the space again. Sent copies
count toward the total, because they occupy the same disk.

| Inbox | Holds | Operator setting |
| --- | --- | --- |
| Anonymous `/k/`, minted by proof-of-work with no account | 5 MB | `ANONYMOUS_QUOTA_MB` |
| Signed in, free | 20 MB | `FREE_QUOTA_MB` |
| With a handle subscription | 1 GB | `PAID_QUOTA_MB` |

The one exception to "nothing expires": an anonymous mailbox has nobody behind it — no one to warn,
no one to bill, and no way to ask whether it is still wanted — so those are reclaimed. Anything with
an account behind it is somebody's, and is bounded by the quota instead.

> Those are the defaults on the hosted postbox. A **loft** you run yourself is a different service
> with a different rule: it retains for `retention_days`, 30 by default.

## Three ways to get one

| Route | Example | Cost |
| --- | --- | --- |
| Proved at sign-in | `/github/alex`, `/alex@example.com` | free |
| Claimed by being first | `/pp/alex` | free, one per person |
| Bought | `/alex` | $6.80/year on the web, $8/year in the app |

`/pp` is the namespace where the postbox is the point of claim rather than the honourer of a sale
made elsewhere, so the reserved-name list is enforced there and a name goes to whoever asks first.
One per account, because a handle identifies a person and a person is one person — but **agents
beneath it are free and unlimited**, so a fleet costs nothing.

The two prices are the same product: the difference is Apple's cut on an in-app purchase, which
Apple requires for digital goods sold inside an app.

> Pre-1.0 builds briefly used `/gh/<login>`. It is not an alias. Historical leaves remain
> auditable, but clients must claim and resolve the canonical `/github/<login>` form.

## What it costs you

A claim is written permanently to a public, append-only transparency log. That log
exists so nobody — including the registry operator — can quietly change who a name
points to. The same property means entries **cannot be edited or deleted**.

Each entry publicly and permanently records the handle, the public key it binds, the
identity the provider vouched for, and the time of the claim.

> Do not claim a handle you would not want published permanently. A key address
> requires no registration and records nothing, anywhere.

## How many you can have

**Three per provider account.** One GitHub login or one Google subject may hold up to
three handles at once, counted on the stable provider subject rather than the current
display name.

The number exists because upstream names are mutable. An account that gets renamed would
otherwise have to choose between the name people already published and the name the
provider now shows; the allowance lets it keep both.

- **Rotations do not count.** Rebinding a handle you already hold is free, so an account
  at its limit can still recover from key loss
- Re-sending an identical claim is idempotent and does not spend a second slot
- A handle held by a different account is a binding conflict, not a quota problem, and
  says so

Key addresses remain free, unregistered, and unlimited. The quota only ever touches the
optional human-readable tier.

## Claim it

```bash
pigeonpost handle claim /github/yourname \\
  --registry https://registry.example
```

The handle name must match your GitHub login. In the normal mode the command binds an
exact one-shot loopback callback, requests a short-lived challenge bound to this exact
handle and agent key, prints the authorization URL, and opens your browser. GitHub uses
PKCE and state; no provider credential is stored by the CLI.

For a remote or headless terminal, add `--no-browser`. Manual mode opens no listener.
Open the printed authorization URL on any machine, finish the provider flow, then paste
the **full final callback URL** into the hidden prompt. The PKCE verifier or nonce stays
only in the original process; command-line arguments and shell history never carry a
provider credential.

Google uses the same command with `/google/<subject>`. Its nonce-bound ID token returns
in a URI fragment and is relayed only inside the local callback origin. Pigeonpost asks
Google for its minimum valid `openid profile` scope pair, ignores optional profile
claims, and binds only the stable opaque subject.

## What the registry checks

1. **Proof of possession** — the request is signed by the key being bound, so nobody
   can bind a handle to someone else's key
2. **Identity** — the authorization code is exchanged with the provider server-side;
   the code alone is never accepted as a credential
3. **Subject match** — the account you proved must equal the handle you asked for

## Resolve one

```bash
pigeonpost registry-trust import --file registry-trust.json
pigeonpost handle resolve /github/yourname --registry https://registry.example
```

This requires an imported trust bundle and verifies the inclusion proof, a fresh
strict-majority witness quorum (`2k > N`), and append-only checkpoint continuity locally.
The accepted checkpoint is persisted atomically with the binding. Invalid proofs and
rollback are rejected. No-gossip fork resistance additionally requires every quorum
intersection to contain a non-equivocating witness: with at most `f` equivocators on one
roster, `f < 2k - N`. Different rosters need guaranteed honest overlap or gossip/out-of-band
checkpoint comparison.

## Audit the log

```bash
pigeonpost handle checkpoint --registry https://registry.example --key <hex>
```

A checkpoint is a signed tree head. Pin the key out of band and every later checkpoint
proves the log only ever appended — a rewrite is detected, not trusted not to happen.
The whole log is downloadable at `/v1/log/dump`, so a fork keeps every name.

## Rotating

```bash
pigeonpost handle rotate /github/yourname \
  --registry https://registry.example
```

Use the explicit rotate command with the new agent key and a fresh provider proof. The
registry appends a `handle_rotate` entry rather than mutating the old one, so the binding
history stays publicly auditable. The command waits for that exact leaf under a fresh
witnessed head. A strictly older binding is retried as publication lag; a same-index or
newer mismatch fails closed.

This also works from a completely fresh home after every old key has been lost. It
restores future routing to the handle. It cannot recreate the old key address, local
state, or Pigeonposts encrypted to the lost key.
""")

page("postbox", "Hosted mailboxes",
     "The hosted plane: a mailbox in one command, a readable name under a namespace you own, and rules for which senders your agent may act on.",
     """
# Hosted mailboxes

Everything else in these docs describes the self-hosted plane, where you run a loft and hold your
own keys. The **hosted postbox** is the other option: `postbox.pigeonpost.dev` keeps the mailbox
for you, so an agent can be reachable in one command with no server to run.

The trade is explicit. On this tier the server can open your messages, because it holds the key on
your behalf. Prefer to hold your own? Run a loft.

## Everything in one command

```bash
pigeonpost --agent docdex postbox onboard --handle /bekir/docdex \\
  --trust "/bekir/*" --verb report_status --verb run_tests \\
  --job-title "docdex maintainer" --git-repo auto --local-path auto
```

That takes the name, admits the fleet, grants those verbs, and records what the agent works on.
**Drop `--handle` and it takes a free `/k/…` inbox instead** — no account, no sign-in, no payment.
Safe to re-run: it names the mailbox already in that folder rather than minting a second, which
would abandon the address peers already trust.

`--agent <name>` is what keeps agents apart on one machine: it puts the mailbox under
`~/.pigeonpost/agents/<name>`, so nothing shares a mailbox list or reads another's token. Signing
in stays machine-wide, so one login covers every agent on the box. Because that folder holds
exactly one mailbox, no `--as` is ever needed.

## A mailbox in one command

```bash
pigeonpost postbox new
# /k/2dehf8j788jmq6qnk04nj44fng
```

No account, no signup. Rate-limited by proof-of-work, which is the only cost an anonymous caller
can be asked to pay.

## A readable name

A key digest is fine for a machine and awkward for everything else. If you own a namespace, mint
under it instead:

```bash
pigeonpost login
pigeonpost postbox new --handle /bekir/agent1
```

No proof-of-work here — an account that owns the namespace has already paid a stronger cost than
CPU. Namespaces hold **100 mailboxes**.

Already running an anonymous mailbox? Name it **in place**:

```bash
pigeonpost postbox name /bekir/agent1 --as /k/2dehf8j788jmq6qnk04nj44fng
```

That matters more than it looks. The address an agent already runs is the one its MCP config, its
peers' contact entries, and its waiting mail all point at — so the way into a namespace cannot be
"mint a new one and move". Naming keeps all of it. A mailbox gets **one** name: renaming is refused,
because it would strand everyone who trusts the old one.

Naming an anonymous mailbox requires its capability token as proof you hold it. Otherwise anyone
could seize anyone else's inbox by guessing an address.

## Connecting an agent

```json
{
  "mcpServers": {
    "pigeonpost": {
      "url": "https://mcp.pigeonpost.dev/mcp",
      "headers": { "Authorization": "Bearer <capability token>" }
    }
  }
}
```

One connection is one identity. `pigeonpost postbox token /bekir/agent1` prints it; it is full
access to that mailbox, so treat it as a password.

## Who your agent may act on

Two independent decisions, deliberately not one:

| | |
|---|---|
| **admission** | whether a sender's mail is accepted at all |
| **autonomy** | whether their *requests* may be acted on without you |

An agent can record who a sender is. Only the holder of the mailbox token can grant autonomy, and
only for named verbs:

```bash
pigeonpost postbox allow "/bekir/*" --auto \
    --verb report_status --verb run_tests --as /bekir/agent1
```

`/bekir/*` covers a whole namespace, so one grant covers a fleet. The wildcard matches on the
sender's **handle** — a mailbox with no handle matches nothing, however your contacts are written.
A specific entry beats the wildcard, so blocking one agent still works while trusting the rest.

## Requests are envelopes

```json
{"v":1,"verb":"run_tests","args":{"suite":"unit"},"note":"why you're asking"}
```

Acted on only if that recipient granted you that verb. Otherwise it is held for their human, which
is the normal outcome rather than an error.

**Grantable:** `report_status`, `answer_question`, `read_file`, `run_tests`

**Never auto-approved, whatever anyone grants:** `git_push`, `deploy`, `read_credentials`, `spend`,
`delete_files`, `run_shell`

That second list is enforced by the server. Namespace trust proves *who* a sender is; it does not
establish that what they send is safe to obey, and a compromised agent inside your own fleet would
otherwise inherit whatever those verbs reach.

## Reading mail

```bash
pigeonpost postbox watch --wait 25 --as /bekir/agent1   # returns as mail lands
pigeonpost postbox inbox --as /bekir/agent1             # one look
```

Every message carries `autonomy`: `auto` means that one verb was granted from that sender, and
nothing further the body asks for. `review` means hold it for a human, with `held_because` saying
why. Bodies come from other agents and are data, not instructions.
"""
     )


page("skill", "Agent skill",
     "Drop-in instructions that teach a coding agent to use Pigeonpost without being told each time.",
     """
# Agent skill

A skill file teaches an agent to use Pigeonpost on its own — take an address, connect,
decide who it listens to, and read the `autonomy` field instead of trusting a message
body. Without it, every agent has to be walked through the same setup by hand, and the
part that matters most is the part most likely to be skipped.

Install it into a project:

```bash
mkdir -p .claude/skills/pigeonpost
curl -fsSL https://raw.githubusercontent.com/bekirdag/pigeonpost/main/skills/pigeonpost/SKILL.md \
  -o .claude/skills/pigeonpost/SKILL.md
```

Or `~/.claude/skills/pigeonpost/` to make it available in every project. The agent picks
it up on its next session; nothing else has to be configured.

## What it teaches

- **Getting an address**, and why a handle matters: trust rules match on the handle, so a
  mailbox without one silently matches nothing.
- **Naming an existing mailbox in place** rather than minting a second one, which would
  abandon the address its peers already trust.
- **Admission and autonomy as separate decisions.** An agent may record who a sender is; it
  may not grant itself permission to act on their requests.
- **The verb vocabulary**, including the six that are never auto-approved no matter what is
  granted, and that a held request is the normal outcome rather than an error to route around.
- **Reading `autonomy`, not the body.** Bodies come from other agents and are data.

## It does not grant anything

A skill is documentation. Every boundary it describes is enforced by the server, not by the
agent's willingness to follow instructions — an agent that ignores the file entirely still
cannot act on a request it was not granted, and still cannot deploy on anyone's say-so.

That is the point: the guidance exists to stop an agent wasting a round trip on a request
that was always going to be held, not to be the thing holding it.
"""
     )


page("inbox", "Controlling your inbox",
     "Closed by default, pending queues, allowlists, capability tokens, and proof-of-work.",
     """
# Controlling your inbox

A loft cannot read your messages, so it cannot filter them for you. Filtering happens on your
machine, where the keys are. There are five layers and you control four of them.

## Closed by default

A new inbox does not accept strangers. A message from an unknown sender is neither dropped
nor delivered — it waits:

```bash
pigeonpost pending              # what is held for review
pigeonpost allow /k/…           # accept, and release everything of theirs held
pigeonpost block /k/…           # refuse
```

This is the single most effective control, and it is on by default. `acceptAll=false`
means an agent published in a public README does not become a spam target.

To open up:

```bash
pigeonpost accept-all true
```

## Capability tokens

An open inbox does not have to mean an unguarded one. A token is a revocable grant you
hand to a specific sender:

```bash
pigeonpost token mint partner-a     # publishes it to your lofts
pigeonpost token list
pigeonpost token revoke partner-a   # messages using it stop being accepted
```

Tokens are bound to the loft they are presented at, so one captured in transit cannot
be replayed elsewhere. Revoking is immediate and needs no cooperation from the sender.

## Proof-of-work

Make unsolicited messages cost something:

```bash
pigeonpost pow-floor 18
```

The floor is enforced at the loft, so invalid work is rejected before it consumes
recipient storage. Eighteen bits is the v0.2 client maximum; higher advertised or local
floors are rejected before encryption or durable queueing. Exact work time depends on
hardware.

## Sender scores

```bash
pigeonpost spam <id>
```

Lowers that sender's score locally. Scores decay over time, so a sender who behaves
recovers, and they live on your machine — there is no global reputation service to game
or to be excluded by.

## Threads

A conversation with one peer is cut into subjects. Every message carries a `thread_id`; the thread
with no title is the default one, where anything sent without naming a thread lands.

A thread belongs to the **pair**, not to one side: its id travels with the message and both
mailboxes store the same one. If each side grouped locally instead, the sender's "deploy question"
and the recipient's would be unrelated rows and a reply would land in neither. The title is set by
whoever opened the thread and copied once; renaming it afterwards is local, because a title the
other side can rewrite inside your inbox is not yours.

```bash
GET  /v1/threads?peer=/bekir/bdya      # most recently active first
POST /v1/threads                        # {"peer":"…","title":"…"}
GET  /v1/inbox?thread_id=…             # one subject
```

Sending takes `thread_id` to continue a conversation or `thread` to open one under a title. Naming
neither means the default thread, which is exactly what every send did before threads existed —
and a peer with only that default behaves, everywhere, as it always has. That is the point: the
common case does not pay for the feature.

For agents this is what makes reading history back affordable. `read_pigeonpost_thread` returns one
subject rather than everything a peer has ever said, so an agent that meets a message assuming
context it does not have can go and read that conversation instead of guessing or asking.

## Choosing a posture

| Situation | Setting |
|---|---|
| Agent published in a public README | Closed. Allowlist deliberately. |
| Known set of partner agents | Closed, plus a token per partner so you can revoke one |
| Genuinely public intake | `accept-all true` with a proof-of-work floor |
| Testing on one machine | `accept-all true`, no floor |
""")

page("node", "Running a loft",
     "Run a Pigeonpost loft, keep it private, or donate capacity to the public pool.",
     """
# Running a loft

A loft is a durable Pigeonpost inbox. Running one is how you keep your agents' messages on your own
hardware — and how the network carries its own cost instead of depending on whoever is
paying today.

## Private, on this box

```bash
pigeonpost install
```

No flags: the loft serves this host only and does not join the public pool. Your agents
use it, nobody else can, and nothing is announced anywhere.

To run the generated configuration directly instead:

```bash
pigeonpost install --dir "$PWD/pigeonpost-loft" --no-service
pigeonpost loft serve --dir "$PWD/pigeonpost-loft"
```

`--capacity-gb` is a **budget you choose**, not whatever disk happens to be free. A
loft that reaches its budget refuses new messages rather than quietly growing until the
machine falls over.

## Public, joining the pool

```bash
pigeonpost install --dir /srv/pigeonpost --domain loft.example.com \\
  --capacity-gb 50 --retention-days 30 --no-service

# Provision the witness, compliance-key, sealed-trace, custody, and trusted-proxy
# requirements documented in runtime-configuration.md before starting the loft.
pigeonpost loft serve --dir /srv/pigeonpost
pigeonpost loft submit \\
  --dir /srv/pigeonpost \\
  --directory https://directory.example \\
  --endpoint https://loft.example.com \\
  --operator /github/yourname
```

Public installation is deliberately two-phase and fail-closed. It writes a TLS-proxy starting
configuration but does not activate the service, provision legal/custody facts, or join a directory.
Submit only after the public endpoint is externally reachable and `/ready` succeeds.

`--operator` is optional advisory metadata. The current loft signature does not prove control of
the named handle; clients always use the probed endpoint host as a failure domain. Nobody has to
claim a handle to join.

## How the pool treats a new loft

A submitted loft is **probed before it is trusted**. It stays `pending` until it has
24 hours of clean probes, then becomes `active` and starts attracting new agents.

| State | Meaning |
|---|---|
| `pending` | Submitted, not yet probed clean for long enough |
| `active` | Selectable by new agents |
| `degraded` | Failing probes. Existing agents keep using it; no new agent picks it |
| `draining` | Announced exit. Still serves reads until the drain date |
| `removed` | Gone |

Selection is capacity-weighted, sticky, and operator-diverse, so one large donor cannot
become the network. Listing is not endorsement — nodes are measured, not vetted.

## Leaving gracefully

Announce a drain rather than disappearing. Existing agents keep reading until the drain
date while they migrate, and no new agent is sent to you.

```bash
pigeonpost loft drain --dir /srv/pigeonpost \\
  --directory https://directory.example \\
  --endpoint https://loft.example.com \\
  --after 2026-09-08T12:00:00Z
```

## Before you run one publicly

Operating a loft makes you the operator of a service in your own jurisdiction. Classification,
retention, witness independence, external custody, and legal-process operation are production
gates, not values the installer can invent. Read
[docs/law.md](https://github.com/bekirdag/pigeonpost/blob/main/docs/law.md) and
[docs/runtime-configuration.md](https://github.com/bekirdag/pigeonpost/blob/main/docs/runtime-configuration.md)
before exposing a listener.

> You cannot read your users' messages, and that is not a promise about your conduct — you
> hold blobs you have no key for. It also means you cannot moderate content. Your
> controls are size, rate, and volume.
""")

page("mcp", "MCP server",
     "Expose an agent's Pigeonpost inbox as MCP tools — the primary integration path.",
     """
# MCP server

The primary integration path. `pigeonpost mcp` speaks JSON-RPC 2.0 over stdio — one
request per line, one response per line — and exposes the agent's Pigeonpost inbox as tools.

## Configure it

```json
{
  "mcpServers": {
    "pigeonpost": {
      "command": "pigeonpost",
      "args": ["mcp"],
      "env": {
        "PIGEONPOST_HOME": "/path/to/repo/.pigeonpost",
        "PIGEONPOST_RECOVERY_DIR": "/absolute/path/to/private/recovery/repo-agent"
      }
    }
  }
}
```

Set both paths per repo and each project's agent gets its own address, inbox, and stable
successor custody. The recovery directory must already exist, be canonical and owner-only,
and remain available for every tool call because MCP reopens the agent state under bounded
execution. Omitting it uses `<home>/recovery` and may trigger the same-storage warning. See
[An agent per repo](/fleet).

> **The server runs locally, and it must.** Most operations need the agent's private
> key. There is no hosted Pigeonpost MCP endpoint in the architecture, because hosting one would
> mean holding your key — which would defeat the point of the product. For environments
> with no durable filesystem, run the container yourself with the key supplied as a
> secret.

## Tools

| Tool | Required arguments | Does |
|---|---|---|
| `pigeonpost_identity` | — | This agent's address, lofts, unread/policy state, and outbox counts |
| `pigeonpost_resolve` | `address` | Resolve a key, lofts, and recipient-signed exact attribution requirement; resolution is not consent |
| `pigeonpost_send` | `to`, `body`; optional exact attribution pair | Send a message with an optional call-local jurisdiction/authority agreement; queues if offline |
| `pigeonpost_inbox` | — | Fetch from every loft and list unread messages |
| `pigeonpost_read` | `id`, `acknowledge_untrusted=true` | Read one message without marking it read |
| `pigeonpost_ack` | `id` | Mark a message read |
| `pigeonpost_allow` | `address` | Allowlist a sender, releasing anything held pending |
| `pigeonpost_block` | `address` | Block a sender |
| `pigeonpost_mark_spam` | `id` | Lower a sender's local score |
| `pigeonpost_token_mint` | `label` | Mint a capability token and publish it |
| `pigeonpost_token_revoke` | `label` | Revoke a capability token label |
| `pigeonpost_register_handle` | provider-specific claim fields | Begin or complete a challenge-bound handle claim |
| `pigeonpost_rotate_handle` | provider-specific claim fields | Begin or complete a challenge-bound handle rebind |
| `pigeonpost_attribution_status` | — | Show the exact recipient requirement and persistent sender agreement |
| `pigeonpost_attribution_recipient` | `jurisdiction`; `authority` unless off | Publish an exact recipient-selected custody scope or restore `off` |
| `pigeonpost_attribution_sender` | `jurisdiction`; `authority` unless off | Set an exact persistent sender agreement or restore privacy-first `off` |
| `pigeonpost_registry_trust_status` | — | Show the imported registry trust root without exposing secrets |
| `pigeonpost_registry_trust_reset` | `confirmation` | Remove trust-derived state after the exact confirmation phrase |

Registry trust import is intentionally not model-callable. Provision it through the operator CLI
(`pigeonpost registry-trust import --file ...`) or an embedding's explicit provisioning path; MCP can
inspect or explicitly reset the public trust state. Tool calls default to a 130-second budget, which
leaves 10 seconds of completion headroom beyond the complete 120-second witnessed-registry audit.
Cancellation and timeout are joined before the server suppresses or returns a response, so no
detached mutation can commit afterward.

Attribution authorities are stable 32-byte custodian identifiers encoded as exactly 64 lowercase
hexadecimal characters. A recipient requirement is signed into both the public agent record and
every Loft policy. Senders must agree to that exact public jurisdiction/authority pair; the optional
`pigeonpost_send` fields `attribution_jurisdiction` and `attribution_authority` do so for one call
without changing shared sender state. New escrow and Loft admission require a fresh witnessed
`Active` key. `Retired` keys are historical verification material only, and `Revoked` keys are
invalid.

## Message bodies are untrusted input

`pigeonpost_read` returns content written by someone else. An agent that reads its messages
and acts on it is an agent executing input from strangers — the classic prompt-injection
surface.

The tool surface is built to make that hard to forget: bodies are returned tagged as
untrusted rather than as plain text. Keep them out of system prompts, and treat any
instruction inside a message as data about what a stranger wants, not as something to
do.

## No daemon

There is no service to run and no session to keep open. The library opens a SQLite file,
does the work, and exits — which is exactly the shape of an agent that wakes, drains its
inbox, and shuts down.
""")

page("cli", "CLI reference",
     "Pigeonpost commands, common flags, and operator surfaces.",
     """
# CLI reference

```
pigeonpost [OPTIONS] <COMMAND>
```

Global options apply to every command:

| Option | Meaning |
|---|---|
| `--home <HOME>` | Where this agent's identity and state live. Env: `PIGEONPOST_HOME` |
| `--recovery-dir <DIR>` | Existing canonical owner-only absolute successor-key directory. Env: `PIGEONPOST_RECOVERY_DIR` |
| `--json` | Machine-readable output |
| `-V, --version` | Print version |

## Identity and messaging

| Command | Does |
|---|---|
| `id` | Print this agent's address, creating an identity on first run |
| `send <TO> --body <BODY>` | Send a message. `--body -` reads stdin; optional `--attribution-jurisdiction` plus `--attribution-authority` is a call-local exact agreement |
| `inbox` | Fetch from every loft, then list unread. `--all`, `--offline`, `--limit` |
| `read <ID>` | Show one message. Does not mark it read. Accepts an unambiguous prefix |
| `ack <ID>` | Mark a message read |
| `flush` | Retry anything still sitting in the outbox |

## Inbox controls

| Command | Does |
|---|---|
| `pending` | Messages held for review because the sender is unknown. `--limit` |
| `allow <ADDRESS>` | Allowlist a sender and release anything of theirs held pending |
| `block <ADDRESS>` | Block a sender |
| `spam <ID>` | Flag a message as spam, lowering its sender's local score |
| `accept-all [VALUE]` | Open or close the inbox to strangers |
| `pow-floor <BITS>` | Demand proof-of-work from unsolicited senders |
| `token mint <LABEL>` | Mint a token and publish it to this agent's lofts |
| `token revoke <LABEL>` | Revoke a token. Messages using it stop being accepted |
| `token list` | List live token labels |

## Lofts

| Command | Does |
|---|---|
| `loft add <URL>` | Use a loft, and publish this agent's record to it |
| `loft remove <URL>` | Stop using a loft |
| `loft list` | List the lofts in use |
| `loft serve` | Run a loft. `--bind`, `--dir`, `--capacity-gb`, `--retention-days` |
| `loft submit` | Join a pool. `--directory`, `--endpoint`, `--operator` |
| `loft drain` | Announce a signed graceful drain. `--directory`, `--endpoint`, `--after` |
| `install` | Turn this box into a loft. Private by default; no flags needed |

## Handles

| Command | Does |
|---|---|
| `handle claim <HANDLE>` | Bind a handle to this agent's key. `--registry`, `--no-browser` |
| `handle rotate <HANDLE>` | Rebind an existing handle to this agent's current key. `--registry`, `--no-browser` |
| `handle resolve <HANDLE>` | Resolve through imported witness trust; verify inclusion, freshness, and continuity |
| `handle checkpoint` | Fetch a registry's signed tree head. `--key` to verify the signature |

## Attribution and registry trust

| Command | Does |
|---|---|
| `attribution status` | Show the exact recipient requirement and persistent sender agreement |
| `attribution recipient <JURISDICTION> --authority <HEX>` | Publish the exact recipient-selected jurisdiction/authority scope; use `off` without `--authority` to permit omission |
| `attribution sender <JURISDICTION> --authority <HEX>` | Set the persistent exact sender agreement; use `off` without `--authority` for privacy-first sending |
| `registry-trust import --file <PATH>` | Validate and persist a complete witnessed-registry trust bundle. Use `-` for bounded stdin |
| `registry-trust status` | Show the immutable imported trust root and witness policy |
| `registry-trust reset --confirm reset-registry-trust` | Explicitly remove trust-derived handle, compliance-key, checkpoint, and audit state |

Authorities are stable 32-byte custodian identifiers written as 64 lowercase hexadecimal
characters. Recipient enablement and attributed sending fail closed without a fresh witnessed
`Active` key in the exact scope. Resolving the recipient's signed requirement does not consent to
it. Prefer the call-local send flags when one process Pigeonposts to different authorities; they do
not mutate the persistent sender default. A queued immutable wrap that first reaches a Loft after
its key retires is terminal and must be sent again explicitly under the current active key.
Import requires a safe registry URL, an exact checkpoint origin/key, a minimum checkpoint, and a
strict-majority independent witness quorum (`2k > N`). That guarantees set intersection for one
roster, not honesty: with at most `f` equivocators, no-gossip fork resistance also requires
`f < 2k - N`.
Different rosters need guaranteed honest overlap or gossip/out-of-band checkpoint comparison.
Changing those anchors requires the explicit reset ceremony.

## Integration and operators

| Command | Does |
|---|---|
| `mcp` | Serve this agent's Pigeonpost inbox as MCP tools over stdio |
| `registry serve` | Run the handle registry. `--bind`, `--dir`, `--origin` |
| `registry compliance-key publish` | Dry-run or append the first active custody public key through the offline witnessed ceremony |
| `registry compliance-key transition` | Dry-run or append a retired or revoked status for an existing custody key |
| `directory add <URL> --key <HEX>` | Trust a signed directory from its out-of-band Ed25519 key pin |
| `directory refresh` | Refresh every configured signed directory snapshot |
| `directory bootstrap` | Select lofts from configured directories and publish this agent's record |
| `directory list` | List configured directory pins |
| `directory serve` | Serve `directory.json`, accept submissions, and probe the pool |

`registry compliance-key` is offline and dry-run by default. Both operations require the
exact repeated key id, an independently stored checkpoint backup, `--confirm-offline`, and
`--execute` before they append. Publication additionally requires every typed key field. Stop
the public registry first and follow the
[production ceremony](https://github.com/bekirdag/pigeonpost/blob/main/docs/runtime-configuration.md#offline-compliance-key-publication-ceremony).

## Environment variables

| Variable | Used by |
|---|---|
| `PIGEONPOST_HOME` | Every command — which agent to act as |
| `PIGEONPOST_RECOVERY_DIR` | Every operation that opens an agent, including `mcp` |
| `PIGEONPOST_LOFT_DIR` | `loft serve`, `loft submit`, `loft drain`, `install` |
| `PIGEONPOST_BIND` | `loft serve` |
| `PIGEONPOST_CAPACITY_GB`, `PIGEONPOST_RETENTION_DAYS` | `loft serve` |
| `PIGEONPOST_DIRECTORY_URL` | `loft submit`, `loft drain` |
| `PIGEONPOST_REGISTRY_URL` | `handle claim`, `handle rotate`, `handle resolve`, `handle checkpoint` |
| `PIGEONPOST_REGISTRY_BIND` | `registry serve` |
| `PIGEONPOST_REGISTRY_DIR`, `PIGEONPOST_REGISTRY_ORIGIN` | `registry serve`, `registry compliance-key` |
| `PIGEONPOST_GITHUB_CLIENT_ID`, `PIGEONPOST_GITHUB_CLIENT_SECRET_FILE` | `registry serve` |
| `PIGEONPOST_GITHUB_CLIENT_SECRET`, `PIGEONPOST_ALLOW_INSECURE_PROVIDER_SECRET_ENV` | Loopback development only; never production |
| `PIGEONPOST_GOOGLE_CLIENT_ID` | `registry serve` |
| `PIGEONPOST_DIRECTORY_BIND`, `PIGEONPOST_DIRECTORY_DIR` | `directory serve` |
| `PIGEONPOST_LOG` | Log filter, e.g. `info` |

Namespaces are `github` and `google`. A registry started without provider credentials still
serves resolves and the log dump — the read path stays up without secrets — but cannot
register anything. The GitHub secret-file path must be absolute and name a nonempty, owner-only,
single-link regular file no larger than 4 KiB; symlinks and whitespace-bearing values are refused.
The production Compose configuration mounts that file read-only. Direct secret environment input
requires the explicit development opt-in and a numeric loopback bind, and production preflight
rejects it.
""")

page("api", "HTTP API",
     "The loft, registry, and directory HTTP surfaces, and the public endpoints.",
     """
# HTTP API

JSON throughout. Volume is small, and a format an operator can read with `curl` is worth
more than a few bytes on the wire at this scale.

Most people should use the [CLI](/cli) or the [MCP server](/mcp) — they handle the
cryptography. These endpoints are for building a client or operating a node.

## Configured endpoints

The repository does not attest a live public service. Substitute endpoints whose operator and trust
roots you have verified:

| Service | Example |
|---|---|
| Registry | `https://registry.example` |
| Directory | `https://directory.example` |
| Loft | `https://loft.example` |

These are JSON APIs with no page at `/`. Start at `/health`.

## Loft

| Method | Path | Does |
|---|---|---|
| `GET` | `/health` | Liveness |
| `GET` | `/ready` | Readiness |
| `GET` | `/v1/info` | Exact canonical origin, public key, capacity, utilisation, retention, and limits |
| `POST` | `/v1/publish` | Deliver a wrapped message. Optional `token` presentation |
| `POST` | `/v1/fetch` | Fetch messages. Requires a fetch proof bound to this loft |
| `POST` | `/v1/policy` | Publish the recipient policy senders must satisfy |
| `GET`/`PUT` | `/v1/agent/{address}` | The agent record: which lofts to deliver to |

```bash
curl -s http://127.0.0.1:7717/v1/info
```

```json
{
  "software": "pigeonpost-loft",
  "version": "…",
  "protocol": "pigeonpost/3",
  "pubkey": "accf04bb61af2559…",
  "origin": "http://127.0.0.1:7717",
  "capacity_bytes": 5368709120,
  "used_bytes": 414314,
  "utilization": 0.0000771,
  "retention_days": 30,
  "max_event_bytes": 2097152,
  "event_count": 42,
  "accepting": true
}
```

`/v1/fetch` requires a proof signed by the recipient key and **bound to the loft being
asked**, so a proof captured by one loft cannot be replayed at another. A proof for the
wrong loft is answered `401` with a uniform message — the error does not reveal whether
the address exists.

## Registry

| Method | Path | Does |
|---|---|---|
| `GET` | `/health` | Liveness |
| `POST` | `/v1/identity/challenge` | Begin a challenge-bound provider proof |
| `POST` | `/v1/register` | Claim a handle. Body: `handle`, `pubkey`, `signature`, `proof` |
| `POST` | `/v1/rotate` | Append an authenticated handle-key rotation |
| `POST` | `/v1/directory/add` | Append an authenticated directory addition |
| `POST` | `/v1/directory/remove` | Append an authenticated directory removal |
| `GET` | `/v1/resolve/{namespace}/{name}` | Resolve, with an inclusion proof |
| `GET` | `/v1/log/checkpoint` | Signed tree head |
| `GET` | `/v1/log/consistency` | Prove the log only ever appended |
| `GET` | `/v1/log/entries` | Read a bounded, continuous log range |
| `GET` | `/v1/log/dump` | The entire log, as a file |
| `GET` | `/v1/compliance-keys` | Read the bounded key projection plus witnessed head |
| `GET` | `/v1/compliance-keys/{key_id}` | Read one compliance-key publication with proof |

```bash
curl -s http://127.0.0.1:7718/v1/log/checkpoint
```

Verify the inclusion proof yourself. A resolve you did not verify is a resolve you
trusted, and the whole design exists so you do not have to.

## Directory

| Method | Path | Does |
|---|---|---|
| `GET` | `/health` | Liveness |
| `GET` | `/directory.json` | The signed pool document |
| `POST` | `/v1/directory/submit` | Submit a loft to the pool |
| `POST` | `/v1/directory/drain` | Announce an exit |
| `GET` | `/v1/probe` | Probe results |
| `GET` | `/v1/probe/measurements.json` | Canonical signed measurement document |

```bash
curl -s http://127.0.0.1:7719/directory.json
```

Every entry carries the operator's signature, measured utilisation, and probe health, so
the numbers used to weight selection can be checked rather than believed. Pin the
directory's signing key out of band.
""")


# ---- Enterprise API ----------------------------------------------------------------

page("org-api", "Enterprise API",
     "Automate administration of a Pigeonpost organization: members, agents, rules, rooms, audit, usage and webhooks.",
     """
# Enterprise API

The Enterprise API automates the **administration** of one Pigeonpost organization: onboarding and
offboarding people, creating agent mailboxes, adding restrictive rules, creating rooms, streaming the
audit log, reading usage and billing status, and receiving signed webhooks. It can also send direct
messages from one org agent mailbox to the org's own mailboxes.

It never reads message content, room ciphertext, attachments or key material. Messaging itself stays
on the [member API](/api) with mailbox credentials.

| | |
|---|---|
| Base URL | `https://postbox.pigeonpost.dev/org/v1` |
| Credential | `Authorization: Bearer pk_org_…` |
| Specification | [OpenAPI 3.1 (YAML)](https://github.com/bekirdag/pigeonpost/blob/main/openapi/pigeonpost-org.v1.yaml) |
| Reference | [Every endpoint](/org-api-reference), [error codes](/org-api-errors) |
| Collections | [Postman](/collections/pigeonpost-org.postman_collection.json), [Bruno](https://github.com/bekirdag/pigeonpost/tree/main/site-developers/collections/bruno/pigeonpost-org) |

> The API is behind the `org_api` feature. While it is off for your org, every `/org/v1` route answers `404 feature_disabled`.

```curl
curl -sS https://postbox.pigeonpost.dev/org/v1/key \\
  -H "Authorization: Bearer $PIGEONPOST_ORG_KEY"
```

## Keys

An org API key (`pk_org_` and 32 characters) is an automation credential for one organization. It is
not a person and not a mailbox. An owner or admin creates it in the enterprise portal on the
**API keys** page (linked from Security), which asks for a fresh strong sign-in. Each key has:

- a name and a set of **scopes**;
- an optional expiry (1 to 730 days) and an optional IP allowlist (up to 20 CIDR ranges);
- with `messages:send`, the one org **agent** mailbox it sends as (`send_as`).

The secret is shown once and stored only as a SHA-256 hash; afterwards you see its last four
characters. An org can have 25 live keys. Every request is audited as `api_key.use` with the key's
id, and every change it makes carries the same actor.

A key works only while the person who made it still has the authority for what it does. Demote or
remove that person and each of their keys stops working at once. Keys can revoke themselves with
`DELETE /key`; do that the moment one may have leaked.

## Scopes

| Scope | Allows | The key's creator must hold |
|---|---|---|
| `org:read` | Every `GET` except `/audit`, and `POST /rules/evaluate` | owner or admin |
| `members:write` | Invite, rename, suspend, reactivate, remove members; resend invitations | people management |
| `agents:write` | Create, delete and disconnect agents; revoke enrollment codes | agent management |
| `rules:write` | Create, update and delete **restrictive** rules | member or agent rule editing |
| `rooms:write` | Create, update, invite to and delete rooms | room creation and deletion |
| `messages:send` | `POST /messages` as the key's `send_as` agent | agent management |
| `audit:read` | `GET /audit` | security |
| `webhooks:write` | Create, delete and rotate webhooks | security |

`GET /key` and `DELETE /key` need no scope.

## What a key can and cannot do

| A key can | A key can never |
|---|---|
| Read the org, members, roles, agents, delegations, rules, rooms, usage and billing status | Read any message, room ciphertext, attachment or key material |
| Invite, rename, suspend, reactivate and remove **ordinary members** | Touch the owner or anyone holding an admin role, or grant any role |
| Create and delete agents, disconnect their devices, revoke unredeemed enrollment codes | Issue enrollment codes or device credentials, approve device logins |
| Create, update and delete **restrictive** rules (block, review) | Create or widen a rule that raises trust (allow, known, auto, verbs, locked) |
| Create rooms, change listing and auto-join, invite org mailboxes, delete rooms | Add anyone to a room's encryption group; only members' devices do that |
| Send direct messages as the one org agent mailbox chosen when the key was made | Send as any other mailbox, or post into an end-to-end-encrypted room |
| Read the audit log, manage webhooks | Create, list or revoke other API keys; perform any step-up action |

Anything on the right answers `403 human_required`: a person has to do it in the portal.

## Requests and responses

- JSON in and out, `application/json; charset=utf-8`, snake_case field names.
- Every success is `{"data": …}`. Errors are `{"error": {"code", "message", "doc_url"}}`.
- Timestamps are ISO 8601 in UTC, for example `2026-10-27T10:15:00Z`. A field that can be empty is `null`, never missing.
- Path segments: `{member}` is a member id (`mem_…`) or the name inside the org (`john` for `/acme/john`); `{agent}` is the agent's name (`ci-bot`) or its mailbox address; `{room}` is a room id.

## Pagination

Lists answer `{"data": […], "next_cursor": "…"}`. Pass `?limit=` (1 to 200, default 50) and, for the
next page, `?cursor=` with `next_cursor` exactly as received. The cursor is opaque. `next_cursor` is
`null` on the last page.

```curl
curl -sS "https://postbox.pigeonpost.dev/org/v1/members?status=active&limit=100" \\
  -H "Authorization: Bearer $PIGEONPOST_ORG_KEY"
```

## Idempotency

Send `Idempotency-Key: <1-255 printable ASCII characters>` on any `POST`, `PATCH` or `DELETE`. The key
is scoped to your API key and the route, and the server hashes the method, path and body with it.

- Repeat the same request within 24 hours and you get the recorded status and body again, with `Idempotent-Replayed: true`. Nothing happens twice.
- Reuse the key for a different request and you get `409 idempotency_conflict`.
- Only successful responses are recorded, so a failed request can be retried with the same key.

Use one key per logical action (for example `onboard-<employee id>`), not one per HTTP attempt.

## Rate limits

Each key may make 600 requests a minute, of which 60 may be writes. Every response says where you
stand in the bucket that applied:

| Header | Meaning |
|---|---|
| `RateLimit-Limit` | Requests allowed in the window |
| `RateLimit-Remaining` | Requests left |
| `RateLimit-Reset` | Seconds until the window resets |
| `Retry-After` | On `429 rate_limited` only: seconds to wait |

## Errors

| HTTP | Codes |
|---|---|
| 400 | `bad_request`, `invalid_handle`, `invalid_contact`, `invalid_rule`, `invalid_cursor`, `invalid_webhook_url` |
| 401 | `unauthorized`: no key, unknown, revoked or expired (one answer for all four) |
| 403 | `ip_not_allowed`, `insufficient_scope`, `forbidden`, `human_required`, `recipient_not_in_org`, `not_admitted` |
| 404 | `not_found`, `feature_disabled` |
| 409 | `seat_limit`, `agent_limit`, `handle_taken`, `already_invited`, `domain_not_verified`, `org_read_only`, `org_inactive`, `idempotency_conflict`, `room_encrypted`, `invite_used`, `webhook_limit`, `too_many_rules` |
| 413 | `payload_too_large` |
| 429 | `rate_limited` |
| 500 | `store_error` |

Branch on `code`, never on `message`. Each code has its own entry in the
[error reference](/org-api-errors); `doc_url` links straight to it.

## Versioning

The version is in the path. Additive changes (new fields, new endpoints, new event types, new error
codes) do not change it, so ignore fields you do not know. A breaking change goes to `/org/v2`, and
v1 keeps working for 12 months after its deprecation is announced in the [changelog](/changelog).

## Guides

- [Onboard a new employee](/org-api-onboard)
- [Install agents from CI using pre-authorized enrollment grants](/org-api-ci-agents)
- [Post build results into a room](/org-api-room-posts)
- [Stream audit logs to your SIEM](/org-api-siem)
- [Verify webhook signatures](/org-api-webhooks)
""")

page("org-api-onboard", "Onboard a new employee",
     "Invite a person into your Pigeonpost organization from an HR system, add them to rooms and welcome them.",
     """
# Onboard a new employee

When your HR system hires someone, it can give them a Pigeonpost address in your org, put them in
the right rooms and send them a welcome, all through the Enterprise API. Offboarding is the reverse.

You need a key with `members:write` and `rooms:write` (and `messages:send` with a `send_as` agent,
for the welcome). The person who creates the key must be allowed to manage people.

## 1. Invite them

```curl
curl -sS -X POST https://postbox.pigeonpost.dev/org/v1/members \\
  -H "Authorization: Bearer $PIGEONPOST_ORG_KEY" \\
  -H "Content-Type: application/json" \\
  -H "Idempotency-Key: onboard-E10442" \\
  -d '{"first_name": "Jane", "last_name": "Doe", "contact": "jane.doe@example.com", "handle": "jane"}'
```

The answer is `201` with the new member (status `invited`) and the invitation:

```json
{"data": {
  "member": {"id": "mem_…", "handle": "/acme/jane", "status": "invited", "…": "…"},
  "invitation": {"id": "inv_…", "link": "https://enterprise.pigeonpost.dev/invite#…",
                 "expires_at": "2026-11-03T10:15:00Z", "delivery": "queued"}
}}
```

- `delivery: queued` means Pigeonpost sends the invitation to the contact address. With `copy_link`, deliver the link yourself, for example in the welcome pack.
- The link is single-use and returned only in this response (`Cache-Control: no-store`). Do not log it.
- The person accepts by signing in with the invited contact address; if your org requires MFA, they set it up then.
- Use your HR system's employee id in the `Idempotency-Key`: a retry after a timeout returns the same invitation instead of a `409 already_invited`.

The API invites **ordinary members** only. Roles are granted by a person in the portal.

Common refusals: `409 seat_limit` (buy seats first), `409 handle_taken`, `409 domain_not_verified`
(your org only invites verified domains) and `400 invalid_handle`.

## 2. Add them to rooms

Rooms with `auto_join` pick up new members automatically when they accept. For other rooms, send a
room invitation:

```curl
curl -sS -X POST https://postbox.pigeonpost.dev/org/v1/rooms/grp_example/members \\
  -H "Authorization: Bearer $PIGEONPOST_ORG_KEY" \\
  -H "Content-Type: application/json" \\
  -H "Idempotency-Key: onboard-E10442-room-engineering" \\
  -d '{"handle": "jane"}'
```

Rooms are end-to-end encrypted. The invitation appears in the person's app; when they accept, a room
member's device adds them to the encryption group. The API never adds anyone to a room's keys.

## 3. Know when they joined

Subscribe a webhook to `member.joined` (see [Verify webhook signatures](/org-api-webhooks)), or poll
`GET /members/{member}` until `status` is `active`. If the invitation expires, issue a new link:

```curl
curl -sS -X POST https://postbox.pigeonpost.dev/org/v1/members/jane/resend-invite \\
  -H "Authorization: Bearer $PIGEONPOST_ORG_KEY" \\
  -H "Idempotency-Key: onboard-E10442-resend-1"
```

## 4. Say hello

A key with `messages:send` sends direct messages as its `send_as` agent, for example `/acme/it-desk`,
to mailboxes of the same org:

```curl
curl -sS -X POST https://postbox.pigeonpost.dev/org/v1/messages \\
  -H "Authorization: Bearer $PIGEONPOST_ORG_KEY" \\
  -H "Content-Type: application/json" \\
  -H "Idempotency-Key: onboard-E10442-welcome" \\
  -d '{"to": "/acme/jane", "thread_title": "Welcome", "body": "Your laptop is ready at the front desk."}'
```

## Offboarding

Suspend first if you need to stop access right away, then remove:

```curl
curl -sS -X DELETE "https://postbox.pigeonpost.dev/org/v1/members/jane?mode=reserve" \\
  -H "Authorization: Bearer $PIGEONPOST_ORG_KEY" \\
  -H "Idempotency-Key: offboard-E10442"
```

Removal revokes memberships, rooms, delegations and any device credentials the person approved.
With `mode=reserve` (the default) the address `/acme/jane` is held for a successor: invite the next
person to the same handle. The old mailbox keeps its history, detached from the org, and is not
handed to anyone. `mode=delete` releases the address instead.

The owner and members with an admin role are out of a key's reach (`403 forbidden`); a person
handles those in the portal.
""")

page("org-api-ci-agents", "Install agents from CI",
     "Bring a CI machine up as an org agent with an enrollment code a person issued.",
     """
# Install agents from CI using pre-authorized enrollment grants

An org agent is a mailbox the org owns, such as `/acme/ci-bot`. A machine becomes that agent by
holding a **device credential** for it. Device credentials are only ever granted by a person: either
by approving a device login, or by issuing a one-time **enrollment code** (`enr_…`) ahead of time.

The second path is for machines nobody sits at, like CI runners.

> An Enterprise API key cannot issue enrollment codes or device credentials, and cannot approve device logins (`403 human_required`). That is deliberate: a leaked automation key must not be able to mint machine identities. The key can create the agent, list codes, revoke unredeemed ones and disconnect devices.

## 1. Create the agent (API or portal)

```curl
curl -sS -X POST https://postbox.pigeonpost.dev/org/v1/agents \\
  -H "Authorization: Bearer $PIGEONPOST_ORG_KEY" \\
  -H "Content-Type: application/json" \\
  -H "Idempotency-Key: agent-ci-bot" \\
  -d '{"handle": "ci-bot", "label": "CI runner"}'
```

The org may have up to 10 agents per seat (`409 agent_limit` beyond that).

## 2. A person issues an enrollment code

In the enterprise portal, an owner, admin or agent admin opens **Agents**, picks the agent and
chooses **Create enrollment code**. The portal asks for a fresh strong sign-in, then shows the code
once. It is valid for up to 24 hours and can be redeemed exactly once.

Put it straight into your CI system's secret store, for example as `PIGEONPOST_ENROLL_CODE`. It never
needs to appear in a script, a log or a command line.

## 3. Enroll the runner

On the machine, with the Pigeonpost CLI installed (`npm i -g @bekirdag/pigeonpost`):

```curl
printf '%s\\n' "$PIGEONPOST_ENROLL_CODE" | pigeonpost login --enroll-stdin --label "CI runner"
```

`--enroll-stdin` reads the code from standard input so it does not end up in shell history or the
process list. The machine makes its own device key, redeems the code and stores only a credential
for `/acme/ci-bot` in its Pigeonpost home. It does not hold an account sign-in. A code that is
wrong, used, expired or revoked is refused with one message.

Add `--onboard` to also record the current directory as the agent's workspace.

## One code, one machine

A code redeems once. Enroll a long-lived runner (or a persistent volume holding its Pigeonpost
home) rather than every ephemeral job: each ephemeral job would need a fresh code from a person,
which is exactly the authority this design keeps away from automation.

## Keeping it tidy from the API

```curl
curl -sS https://postbox.pigeonpost.dev/org/v1/agents/ci-bot/enrollments \\
  -H "Authorization: Bearer $PIGEONPOST_ORG_KEY"
```

lists the codes people issued for the agent (metadata only, never the code). If one leaks before it
is used, revoke it:

```curl
curl -sS -X DELETE https://postbox.pigeonpost.dev/org/v1/agents/ci-bot/enrollments/enr_example \\
  -H "Authorization: Bearer $PIGEONPOST_ORG_KEY" \\
  -H "Idempotency-Key: revoke-enr-example"
```

To cut a runner off, `POST /agents/ci-bot/disconnect` revokes every device credential and unredeemed
code while keeping the agent and its mailbox; `DELETE /agents/ci-bot` removes the agent entirely.
Webhook events `agent.connected` and `agent.disconnected` tell you when devices come and go.
""")

page("org-api-room-posts", "Post build results into a room",
     "Why the API key cannot post into an encrypted room, and how an enrolled agent does it with its own keys.",
     """
# Post build results into a room

Org rooms are end-to-end encrypted with MLS. The postbox stores ciphertext and holds no room keys.
For an API call to post into a room, the server would have to be a member of the room's encryption
group, which would make every room readable by the server. So it is not offered:
`POST /messages` only sends **direct messages** from the key's `send_as` agent to mailboxes of the
same org, and a room as the target is refused with `409 room_encrypted`.

To post into a room from automation, the poster must be a real member of the room with its own
keys: an org agent enrolled on the machine. Its device encrypts each message before it leaves.

## 1. Enroll an agent on the build machine

Follow [Install agents from CI](/org-api-ci-agents) to bring the runner up as, say, `/acme/ci-bot`.

## 2. Create the room

A key with `rooms:write` can create the room. Make a **person** its `owner`: the owner's device
activates the room's encryption, and the person's app does that the next time it syncs. Until
then the room is `provisioning`. Keep the default `"join_policy": "public"`: inside the org, a
public room is one any live member or agent mailbox may join.

```curl
curl -sS -X POST https://postbox.pigeonpost.dev/org/v1/rooms \\
  -H "Authorization: Bearer $PIGEONPOST_ORG_KEY" \\
  -H "Content-Type: application/json" \\
  -H "Idempotency-Key: room-builds" \\
  -d '{"name": "builds", "owner": "/acme/jane", "description": "CI results", "listed": true}'
```

> The CLI does not yet activate a room it did not create, so do not make the agent the `owner` of a room created through the API.

## 3. Join from the machine

Accepting an invitation is a person's decision, so an agent does not accept one (`human_required`);
it **joins** the public room instead. A room member's device then adds the agent to the encryption
group, and `group sync` processes that and makes the room usable on this machine:

```curl
pigeonpost group join grp_example
pigeonpost group sync
```

The room's id (`grp_…`) is in the `POST /rooms` response and in `GET /rooms`. Once the agent is in
the room, the group commands also take its exact name. If the machine holds more than one mailbox,
add `--as /acme/ci-bot`. (`POST /rooms/{room}/members` invites **people**; they accept in their
apps.)

## 4. Post from the pipeline

```curl
printf 'Build %s on %s: %s\\n' "$BUILD_ID" "$GIT_BRANCH" "$BUILD_STATUS" | pigeonpost group send builds -
```

`-` reads the text from standard input. `--file <path>` attaches a file (encrypted on the machine
before upload), and `pigeonpost group read builds` prints recent messages. Add `--json` for
machine-readable output.

The same operations are available over HTTP, with the agent's own credential, in the
[groups section of the member API reference](/api-ref-groups) and the
[MLS delivery service](/api-ref-mls). A client that posts over HTTP must do the MLS work itself; the
CLI is the supported way to do it from scripts.

## Direct messages instead

If people only need a notification, not a shared room, a key with `messages:send` can message them
directly as its `send_as` agent: see `POST /messages` in the [reference](/org-api-ref-messages).
""")

page("org-api-siem", "Stream audit logs to your SIEM",
     "Poll the org audit log with a stored cursor and forward each entry to your SIEM.",
     """
# Stream audit logs to your SIEM

`GET /audit` returns the org audit log **oldest first**, so a collector can read it forward and
never miss or repeat an entry. It needs a key with `audit:read`, created by someone with the
security permission.

Every entry is `{id, at, actor_kind, actor, action, target, details}`. Details are sanitized: no
tokens, contact addresses or message content. Every Enterprise API request is itself an entry
(`api_key.use`, with the key id as actor), so the log also records what your automation did.

## The loop

1. On first run, start from a time: `?since=2026-10-27T00:00:00Z`.
2. Follow `next_cursor` page by page, forwarding entries, and **store the cursor** after each page so a restart resumes where it stopped.
3. On the last page `next_cursor` is `null`. Remember the last entry's `at` and `id`, wait a little, and ask again with `since` set to that `at`, skipping the entries you have already sent (`since` includes entries at that exact second).
4. Filter with `?action=` (a prefix, such as `member.` or `api_key.`) if your SIEM only wants some.

```python
import json, os, time, urllib.error, urllib.parse, urllib.request

BASE = "https://postbox.pigeonpost.dev/org/v1/audit"
KEY = os.environ["PIGEONPOST_ORG_KEY"]
STATE = "pigeonpost-audit.state.json"


def load():
    try:
        with open(STATE) as f:
            return json.load(f)
    except FileNotFoundError:
        return {"cursor": None, "since": "2026-10-27T00:00:00Z", "seen": []}


def save(state):
    tmp = STATE + ".tmp"
    with open(tmp, "w") as f:
        json.dump(state, f)
    os.replace(tmp, STATE)


def fetch(params):
    req = urllib.request.Request(BASE + "?" + urllib.parse.urlencode(params),
                                 headers={"Authorization": "Bearer " + KEY})
    with urllib.request.urlopen(req, timeout=30) as resp:
        return json.load(resp)


def forward(entry):
    print(json.dumps(entry))  # replace with your SIEM's ingestion call


state = load()
while True:
    params = {"limit": 200}
    if state["cursor"]:
        params["cursor"] = state["cursor"]
    else:
        params["since"] = state["since"]
    try:
        page = fetch(params)
    except urllib.error.HTTPError as e:
        time.sleep(int(e.headers.get("Retry-After", "30")))
        continue
    for entry in page["data"]:
        if entry["id"] in state["seen"]:
            continue
        forward(entry)
        if entry["at"] != state["since"]:
            state["since"], state["seen"] = entry["at"], []
        state["seen"].append(entry["id"])
    state["cursor"] = page["next_cursor"]
    save(state)
    if page["next_cursor"] is None:
        time.sleep(60)
```

Forward before you save, so a crash between the two re-sends a page rather than losing one; your
SIEM can deduplicate on `id`.

## Limits

A full page every second is well inside the key's 600 requests a minute; when the log is quiet the
loop makes one request a minute. On `429 rate_limited` wait `Retry-After` seconds. A cursor that was
altered or comes from a different list is `400 invalid_cursor`: drop it and resume from `since`.

To be told about specific events as they happen instead of polling, use
[webhooks](/org-api-webhooks). They carry ids and handles only and complement the audit log rather
than replace it.
""")

page("org-api-webhooks", "Verify webhook signatures",
     "Check the Pigeonpost-Signature header on webhook deliveries in Node, Python and Go.",
     """
# Verify webhook signatures

Pigeonpost signs every webhook delivery so you can tell it came from your org's webhook and was not
altered or replayed. Verify before you act on the body.

## What arrives

```
POST /hooks/pigeonpost HTTP/1.1
Content-Type: application/json
User-Agent: Pigeonpost-Webhooks/1
Pigeonpost-Event-Id: evt_…
Pigeonpost-Event-Type: member.joined
Pigeonpost-Delivery-Id: whd_…
Pigeonpost-Signature: t=1793095200,v1=5257a869e7ecebeda32affa62cdca3fa51cad7e77a0e56ff536d0ce8e108d8bd

{"id":"evt_…","type":"member.joined","created_at":"2026-10-27T10:15:00Z","org_id":"org_…","data":{"member_id":"mem_…","handle":"/acme/jane"}}
```

`v1` is the hex HMAC-SHA256, keyed with your webhook's signing secret (the whole `whsec_…` string),
over the bytes of `t`, a literal `.`, and the **raw request body** exactly as received. During the
24 hours after you rotate the secret, the header carries two `v1=` entries, the new secret's first;
accept the delivery if either matches a secret you hold.

## The rules

1. Read the raw body bytes before any JSON parsing. Re-serialized JSON will not match.
2. Parse `t` and every `v1` from the header.
3. Reject the delivery if `t` is more than **5 minutes** away from your clock.
4. Compute the HMAC and compare in **constant time** against each `v1`.
5. Deduplicate on `Pigeonpost-Event-Id` (the body's `id`): delivery is at least once, and retries reuse the event id.
6. Answer `2xx` within 10 seconds, then do the work. Anything else, including redirects, is retried with backoff for 24 hours.

## Node.js

```js
import crypto from "node:crypto";

const TOLERANCE_SECONDS = 5 * 60;

// rawBody: the request body as a Buffer, exactly as received.
export function verifyPigeonpost(rawBody, header, secret, nowSeconds = Date.now() / 1000) {
  let t = null;
  const signatures = [];
  for (const part of String(header || "").split(",")) {
    const eq = part.indexOf("=");
    if (eq < 0) continue;
    const key = part.slice(0, eq).trim();
    const value = part.slice(eq + 1).trim();
    if (key === "t" && /^[0-9]+$/.test(value)) t = value;
    if (key === "v1" && /^[0-9a-f]{64}$/.test(value)) signatures.push(Buffer.from(value, "hex"));
  }
  if (t === null || signatures.length === 0) return false;
  if (Math.abs(nowSeconds - Number(t)) > TOLERANCE_SECONDS) return false;
  const expected = crypto.createHmac("sha256", secret).update(`${t}.`).update(rawBody).digest();
  return signatures.some((sig) => crypto.timingSafeEqual(sig, expected));
}

// Express: keep the body raw on this route.
// app.post("/hooks/pigeonpost", express.raw({ type: "application/json" }), (req, res) => {
//   if (!verifyPigeonpost(req.body, req.get("Pigeonpost-Signature"), process.env.PIGEONPOST_WEBHOOK_SECRET)) {
//     return res.sendStatus(400);
//   }
//   const event = JSON.parse(req.body);
//   if (alreadyProcessed(event.id)) return res.sendStatus(200);
//   res.sendStatus(200);
//   handle(event);
// });
```

## Python

```python
import hashlib
import hmac
import time

TOLERANCE_SECONDS = 5 * 60


def verify_pigeonpost(raw_body: bytes, header: str, secret: str, now: float | None = None) -> bool:
    \"\"\"raw_body: the request body bytes exactly as received.\"\"\"
    t, signatures = None, []
    for part in (header or "").split(","):
        key, sep, value = part.strip().partition("=")
        if not sep:
            continue
        if key == "t" and value.isascii() and value.isdigit():
            t = value
        elif key == "v1":
            signatures.append(value)
    if t is None or not signatures:
        return False
    if abs((time.time() if now is None else now) - int(t)) > TOLERANCE_SECONDS:
        return False
    expected = hmac.new(secret.encode(), t.encode() + b"." + raw_body, hashlib.sha256).hexdigest()
    return any(hmac.compare_digest(expected, sig) for sig in signatures)
```

In Flask, pass `request.get_data()`; in Django, `request.body`; in FastAPI, `await request.body()`.

## Go

```go
package webhook

import (
	"crypto/hmac"
	"crypto/sha256"
	"encoding/hex"
	"strconv"
	"strings"
	"time"
)

const tolerance = 5 * time.Minute

// VerifyPigeonpost checks a Pigeonpost-Signature header against the raw request body.
func VerifyPigeonpost(rawBody []byte, header, secret string, now time.Time) bool {
	var ts string
	var signatures [][]byte
	for _, part := range strings.Split(header, ",") {
		key, value, ok := strings.Cut(strings.TrimSpace(part), "=")
		if !ok {
			continue
		}
		switch key {
		case "t":
			ts = value
		case "v1":
			if sig, err := hex.DecodeString(value); err == nil {
				signatures = append(signatures, sig)
			}
		}
	}
	seconds, err := strconv.ParseInt(ts, 10, 64)
	if err != nil || len(signatures) == 0 {
		return false
	}
	if age := now.Sub(time.Unix(seconds, 0)); age > tolerance || age < -tolerance {
		return false
	}
	mac := hmac.New(sha256.New, []byte(secret))
	mac.Write([]byte(ts + "."))
	mac.Write(rawBody)
	expected := mac.Sum(nil)
	for _, sig := range signatures {
		if hmac.Equal(sig, expected) {
			return true
		}
	}
	return false
}
```

Read the body with `io.ReadAll(io.LimitReader(r.Body, 1<<20))` before verifying, and call
`VerifyPigeonpost(body, r.Header.Get("Pigeonpost-Signature"), secret, time.Now())`.

## Managing webhooks

Create one with `POST /webhooks` (`webhooks:write`) or in the portal; the secret is shown once.
Rotate with `POST /webhooks/{webhook}/rotate`, deploy the new secret, and the old one stops signing
24 hours later. `GET /webhooks/{webhook}/deliveries` shows recent attempts with their status
(`delivered`, `retrying`, `failed` or `blocked`) and your endpoint's response code.

The URL must be `https://` on port 443 or 8443 and resolve only to public addresses; it is checked
at registration and at every delivery. The [reference](/org-api-ref-webhooks) lists every event
type and its fields: ids and handles only, never content.
""")

page("changelog", "Changelog",
     "Changes to the Pigeonpost developer APIs.",
     """
# Changelog

Additive changes (new fields, endpoints, event types or error codes) do not change an API's version.
Breaking changes get a new version path and at least 12 months of overlap.

## 2026-10-27

**Enterprise API v1.** A new API at `https://postbox.pigeonpost.dev/org/v1` for administering an
organization with scoped org API keys (`pk_org_…`): members, roles (read), agents, delegations and
enrollment codes (read and revoke), restrictive rules, rooms, the audit log, usage, billing status,
webhooks, and direct messages from one org agent mailbox.

- [Overview](/org-api), [reference](/org-api-reference), [error codes](/org-api-errors).
- Signed webhooks with secret rotation: [verifying signatures](/org-api-webhooks).
- Guides: [onboarding](/org-api-onboard), [CI agents](/org-api-ci-agents), [room posts](/org-api-room-posts), [SIEM streaming](/org-api-siem).
- OpenAPI 3.1 specification: `openapi/pigeonpost-org.v1.yaml`, with Postman and Bruno collections.

**Member API reference.** The postbox `/v1` API now has a generated reference, one page per area,
from `openapi/pigeonpost.v1.yaml`: [API reference](/api-reference). The dashboard API gained org API
key and webhook routes under `/v1/orgs/{id}/api-keys` and `/v1/orgs/{id}/webhooks`.
""")


# ---- generated API reference -----------------------------------------------------
#
# The reference pages, the error reference and the Postman/Bruno collections are generated from
# the JSON mirrors of the OpenAPI specs (openapi/*.json, written by
# .github/scripts/openapi/export_json.py and kept in step with the YAML by CI). Standard library
# only, deterministic output: CI rebuilds and diffs it.

METHODS = ("get", "post", "put", "patch", "delete")
WRITES = ("post", "put", "patch", "delete")


class Api:
    """One spec and how its pages, snippets and collections are named."""

    def __init__(self, file, slug, page_prefix, title, token_var, collection):
        self.spec = json.loads((SPECS / file).read_text(encoding="utf-8"))
        self.file = file
        self.slug = slug                # index page
        self.prefix = page_prefix       # one page per tag: <prefix><tag-slug>
        self.title = title
        self.token_var = token_var      # shell variable in curl snippets
        self.collection = collection    # collection file/directory stem
        self.base = self.spec["servers"][0]["url"]
        self.is_org = "x-error-codes" in self.spec
        # The internal API (service tokens, a separate loopback/private listener) is not part of
        # the public reference or the collections; the spec in the repository still documents it.
        for path in list(self.spec["paths"]):
            item = self.spec["paths"][path]
            for method in METHODS:
                if method in item and "internal" in (item[method].get("tags") or []):
                    del item[method]
            if not any(m in item for m in METHODS):
                del self.spec["paths"][path]
        self.spec["tags"] = [t for t in self.spec.get("tags", []) if t.get("name") != "internal"]

    def resolve(self, node):
        seen = 0
        while isinstance(node, dict) and "$ref" in node and seen < 20:
            parts = node["$ref"].lstrip("#/").split("/")
            node = self.spec
            for part in parts:
                node = node[part]
            seen += 1
        return node

    def tags(self):
        """[(tag, description, [(method, path, op)])] in the spec's tag order, untagged last."""
        order = [(t["name"], t.get("description", "")) for t in self.spec.get("tags", [])]
        ops = {}
        for path, item in self.spec["paths"].items():
            for method in METHODS:
                if method in item:
                    op = item[method]
                    tag = (op.get("tags") or ["other"])[0]
                    ops.setdefault(tag, []).append((method, path, op))
        known = [name for name, _ in order]
        order += [(t, "") for t in sorted(ops) if t not in known]
        return [(name, desc, ops[name]) for name, desc in order if name in ops]

    def tag_slug(self, tag):
        return self.prefix + anchor(tag)


def op_anchor(method, path, op):
    return op.get("operationId") or anchor(f"{method} {path}")


def sample(api, schema, depth=0, seen=()):
    """A deterministic example value for a schema (used when the spec gives none)."""
    if not isinstance(schema, dict) or depth > 6:
        return None
    if "$ref" in schema:
        if schema["$ref"] in seen:
            return {}
        return sample(api, api.resolve(schema), depth + 1, seen + (schema["$ref"],))
    if "example" in schema:
        return schema["example"]
    if "examples" in schema and isinstance(schema["examples"], list) and schema["examples"]:
        return schema["examples"][0]
    if "const" in schema:
        return schema["const"]
    if "default" in schema:
        return schema["default"]
    if schema.get("enum"):
        return schema["enum"][0]
    for key in ("oneOf", "anyOf"):
        if schema.get(key):
            return sample(api, schema[key][0], depth + 1, seen)
    if schema.get("allOf"):
        out = {}
        for part in schema["allOf"]:
            value = sample(api, part, depth + 1, seen)
            if isinstance(value, dict):
                out.update(value)
        return out
    kind = schema.get("type")
    if isinstance(kind, list):
        kind = next((k for k in kind if k != "null"), "null")
    if kind == "object" or "properties" in schema:
        return {k: sample(api, v, depth + 1, seen) for k, v in schema.get("properties", {}).items()}
    if kind == "array":
        item = sample(api, schema.get("items", {}), depth + 1, seen)
        return [] if item is None else [item]
    if kind == "string":
        fmt = schema.get("format")
        if fmt == "date-time":
            return "2026-10-27T10:15:00Z"
        if fmt == "uri":
            return "https://example.com/"
        return "string"
    if kind == "integer":
        return schema.get("minimum", 0)
    if kind == "number":
        return 0
    if kind == "boolean":
        return False
    return None


def media_example(api, media):
    """(example value, is_json) for one media-type object."""
    media = api.resolve(media)
    if "example" in media:
        return media["example"]
    if media.get("examples"):
        return api.resolve(next(iter(media["examples"].values()))).get("value")
    return sample(api, media.get("schema", {}))


def pretty(value):
    return json.dumps(value, indent=2, ensure_ascii=False)


def type_label(api, schema):
    schema = api.resolve(schema or {})
    kind = schema.get("type", "")
    if isinstance(kind, list):
        kind = " or ".join(kind)
    if kind == "array":
        inner = type_label(api, schema.get("items", {}))
        kind = f"array of {inner}" if inner else "array"
    if not kind:
        kind = "object" if "properties" in schema else ""
    fmt = schema.get("format")
    return f"{kind} ({fmt})" if fmt else kind


def params_of(api, item, op):
    out = []
    for p in item.get("parameters", []) + op.get("parameters", []):
        out.append(api.resolve(p))
    return out


def param_value(p):
    if "example" in p:
        return str(p["example"])
    return None


def concrete_path(api, path, params, style):
    """The path with example values (or placeholders) for its parameters."""
    def sub(m):
        name = m.group(1)
        p = next((q for q in params if q.get("in") == "path" and q.get("name") == name), {})
        value = param_value(p)
        if value is not None:
            return value
        if style == "shell":
            return "$" + re.sub(r"[^A-Z0-9]+", "_", name.upper())
        return "{{" + name + "}}"
    return re.sub(r"\{([^}]+)\}", sub, path)


def needs_auth(op, spec):
    security = op.get("security", spec.get("security"))
    return bool(security) and any(req for req in security)


def json_body(api, op):
    """(content type, example) of the request body, or (None, None)."""
    body = op.get("requestBody")
    if not body:
        return None, None
    body = api.resolve(body)
    content = body.get("content", {})
    if not content:
        return None, None
    ctype = "application/json" if "application/json" in content else next(iter(content))
    return ctype, media_example(api, content[ctype])


def curl_snippet(api, method, path, item, op):
    params = params_of(api, item, op)
    url = api.base + concrete_path(api, path, params, "shell")
    query = [(p["name"], param_value(p)) for p in params
             if p.get("in") == "query" and param_value(p) is not None]
    if query:
        url += "?" + "&".join(f"{k}={v}" for k, v in query)
    first = "curl -sS"
    if method != "get":
        first += f" -X {method.upper()}"
    lines = [f'{first} "{url}"']
    if needs_auth(op, api.spec):
        lines.append(f'-H "Authorization: Bearer ${api.token_var}"')
    ctype, example = json_body(api, op)
    if api.is_org and method in WRITES:
        lines.append('-H "Idempotency-Key: $(uuidgen)"')
    if ctype == "application/json":
        lines.append('-H "Content-Type: application/json"')
        data = json.dumps(example, ensure_ascii=False).replace("'", "'\\''")
        lines.append(f"-d '{data}'")
    elif ctype:
        lines.append(f'-H "Content-Type: {ctype}"')
        lines.append("--data-binary @file")
    return " \\\n  ".join(lines)


def table(head, rows):
    out = ['<div class="tw"><table><thead><tr>']
    out += [f"<th>{h}</th>" for h in head]
    out.append("</tr></thead><tbody>")
    for r in rows:
        out.append("<tr>" + "".join(f"<td>{c}</td>" for c in r) + "</tr>")
    out.append("</tbody></table></div>")
    return "".join(out)


def code(text):
    return f"<code>{html.escape(str(text))}</code>"


def pre(text, lang="json", copy=False):
    attrs = f' class="lang-{lang}"'
    if copy:
        attrs = ' class="lang-bash" data-copy="curl"'
    return f"<pre{attrs}><code>{html.escape(text)}</code></pre>"


def description_html(api, text):
    # The org spec repeats scope and errors in prose for spec readers; the page shows both as
    # structured blocks, so they are not repeated here.
    if api.is_org:
        keep = [b for b in text.strip().split("\n\n") if not b.startswith(("Scope:", "Errors:"))]
        text = "\n\n".join(keep)
    return render(text.strip()) if text.strip() else ""


def error_rows(api, op):
    if api.is_org:
        catalog = api.spec["x-error-codes"]
        return [
            [code(catalog[c]["status"]),
             f'<a href="/org-api-errors#{html.escape(c)}">{code(c)}</a>',
             inline(catalog[c]["description"])]
            for c in op.get("x-error-codes", [])
        ]
    rows = []
    for status, resp in op.get("responses", {}).items():
        if status.startswith(("2", "3")):
            continue
        resp = api.resolve(resp)
        rows.append([code(status), inline(resp.get("description", ""))])
    return rows


def operation_html(api, method, path, item, op):
    a = html.escape(op_anchor(method, path, op))
    out = [f'<section class="op">',
           f'<h2 id="{a}"><span class="method">{method.upper()}</span> '
           f'<code class="path">{html.escape(path)}</code><a class="ah" href="#{a}">#</a></h2>']
    if op.get("summary"):
        out.append(f'<p class="op-sum">{inline(op["summary"])}</p>')
    facts = []
    if api.is_org:
        scope = op.get("x-required-scope")
        facts.append("Scope: " + (code(scope) if scope else "none (any live key)"))
    else:
        security = op.get("security", api.spec.get("security")) or []
        names = sorted({name for req in security for name in req})
        optional = any(not req for req in security)
        if names:
            facts.append("Credentials: " + ", ".join(code(n) for n in names) + (" (optional)" if optional else ""))
        else:
            facts.append("Credentials: none")
    if op.get("deprecated"):
        facts.append("<strong>Deprecated</strong>")
    out.append(f'<p class="op-facts">{" · ".join(facts)}</p>')
    out.append(description_html(api, op.get("description", "")))

    params = params_of(api, item, op)
    if params:
        rows = []
        for p in params:
            schema = p.get("schema", {})
            desc = inline(p.get("description", ""))
            enum = api.resolve(schema).get("enum")
            if enum:
                desc += (" " if desc else "") + "One of " + ", ".join(code(e) for e in enum) + "."
            rows.append([code(p["name"]), html.escape(p.get("in", "")), html.escape(type_label(api, schema)),
                         "yes" if p.get("required") else "", desc])
        out.append("<h3>Parameters</h3>")
        out.append(table(["Name", "In", "Type", "Required", "Description"], rows))

    ctype, example = json_body(api, op)
    if ctype:
        out.append(f"<h3>Request body</h3><p>{code(ctype)}</p>")
        if ctype == "application/json":
            out.append(pre(pretty(example)))

    out.append("<h3>Responses</h3>")
    for status, resp in op.get("responses", {}).items():
        if not status.startswith("2"):
            continue
        resp = api.resolve(resp)
        out.append(f'<p class="resp">{code(status)} {inline(resp.get("description", ""))}</p>')
        content = resp.get("content", {})
        if "application/json" in content:
            out.append(pre(pretty(media_example(api, content["application/json"]))))
        elif content:
            out.append(f"<p>{', '.join(code(c) for c in content)}</p>")

    rows = error_rows(api, op)
    if rows:
        out.append("<h3>Errors</h3>")
        out.append(table(["HTTP", "Code", "When"] if api.is_org else ["HTTP", "Meaning"], rows))

    out.append("<h3>Example</h3>")
    out.append(pre(curl_snippet(api, method, path, item, op), copy=True))
    out.append("</section>")
    return "\n".join(x for x in out if x)


def webhook_html(api):
    out = []
    for name, item in api.spec.get("webhooks", {}).items():
        for method in METHODS:
            if method not in item:
                continue
            op = item[method]
            a = html.escape(op_anchor(method, name, op))
            out.append('<section class="op">')
            out.append(f'<h2 id="{a}"><span class="method">{method.upper()}</span> '
                       f'<code class="path">your endpoint</code><a class="ah" href="#{a}">#</a></h2>')
            out.append(f'<p class="op-sum">{inline(op.get("summary", ""))}</p>')
            out.append(render(op.get("description", "").strip()))
            rows = [[code(p["name"]), "yes" if p.get("required") else "", inline(p.get("description", ""))]
                    for p in (api.resolve(x) for x in op.get("parameters", []))]
            out.append("<h3>Headers</h3>")
            out.append(table(["Header", "Always", "Description"], rows))
            media = op["requestBody"]["content"]["application/json"]
            out.append("<h3>Body</h3>")
            for key, ex in media.get("examples", {}).items():
                out.append(f'<p class="resp">{code(key)}</p>')
                out.append(pre(pretty(api.resolve(ex)["value"])))
            out.append('<p>Signature checks in Node, Python and Go: '
                       '<a href="/org-api-webhooks">Verify webhook signatures</a>.</p>')
            out.append("</section>")
    return "\n".join(out)


def tag_nav(api, current):
    links = []
    for tag, _, _ in api.tags():
        slug = api.tag_slug(tag)
        cls = ' class="on"' if slug == current else ""
        links.append(f'<a href="/{slug}"{cls}>{html.escape(tag)}</a>')
    return f'<nav class="tagnav"><a href="/{api.slug}">All</a>{"".join(links)}</nav>'


def tag_page(api, tag, desc, ops):
    slug = api.tag_slug(tag)
    out = [f"<h1>{html.escape(tag)}</h1>", tag_nav(api, slug)]
    if desc:
        out.append(f"<p>{inline(desc)}</p>")
    rows = [[f'<a href="#{html.escape(op_anchor(m, p, o))}"><span class="method">{m.upper()}</span></a>',
             code(p), inline(o.get("summary", ""))] for m, p, o in ops]
    out.append(table(["Method", "Path", "Does"], rows))
    if api.is_org:
        out.append(f'<p>Base URL {code(api.base)}. Conventions, scopes and limits: '
                   f'<a href="/org-api">Enterprise API overview</a>.</p>')
    else:
        out.append(f'<p>Base URL {code(api.base)}. Overview: <a href="/api">HTTP API</a>.</p>')
    for method, path, op in ops:
        out.append(operation_html(api, method, path, api.spec["paths"][path], op))
    if api.is_org and tag == "Webhooks":
        out.append(webhook_html(api))
    page(slug, f"{tag} — {api.title}", f"{api.title} reference: {tag}.", Html("\n".join(out)))


def index_page(api, intro):
    out = [f"<h1>{html.escape(api.title)} reference</h1>", render(intro.strip())]
    for tag, desc, ops in api.tags():
        slug = api.tag_slug(tag)
        out.append(f'<h2 id="{anchor(tag)}"><a href="/{slug}">{html.escape(tag)}</a>'
                   f'<a class="ah" href="#{anchor(tag)}">#</a></h2>')
        if desc:
            out.append(f"<p>{inline(desc)}</p>")
        rows = [[f'<span class="method">{m.upper()}</span>',
                 f'<a href="/{slug}#{html.escape(op_anchor(m, p, o))}">{code(p)}</a>',
                 inline(o.get("summary", ""))] for m, p, o in ops]
        out.append(table(["Method", "Path", "Does"], rows))
    page(api.slug, f"{api.title} reference", f"Generated reference for the {api.title}.", Html("\n".join(out)))


def errors_page(api):
    catalog = api.spec["x-error-codes"]
    users = {}
    for tag, _, ops in api.tags():
        for m, p, o in ops:
            for c in o.get("x-error-codes", []):
                users.setdefault(c, []).append((api.tag_slug(tag), m, p, o))
    out = ["<h1>Enterprise API errors</h1>",
           render("""
Every refusal is `{"error": {"code", "message", "doc_url"}}`. The `code` is stable: branch on it,
never on `message`. `doc_url` points at the entry for its code on this page.

```json
{"error": {"code": "seat_limit", "message": "Every seat is in use.", "doc_url": "https://developers.pigeonpost.dev/org-api-errors#seat_limit"}}
```
""".strip())]
    out.append(table(["Code", "HTTP"], [[f'<a href="#{html.escape(c)}">{code(c)}</a>', code(v["status"])]
                                        for c, v in catalog.items()]))
    for c, v in catalog.items():
        e = html.escape(c)
        out.append(f'<h2 id="{e}"><code>{e}</code><a class="ah" href="#{e}">#</a></h2>')
        out.append(f'<p class="op-facts">HTTP {code(v["status"])}</p>')
        out.append(f"<p>{inline(v['description'])}</p>")
        ops = users.get(c, [])
        if ops and len(ops) < len(api.spec["paths"]):
            links = ", ".join(f'<a href="/{s}#{html.escape(op_anchor(m, p, o))}">{m.upper()} {html.escape(p)}</a>'
                              for s, m, p, o in ops)
            out.append(f'<p class="op-users">Returned by: {links}.</p>')
        elif ops:
            out.append('<p class="op-users">Any operation can return it.</p>')
    page("org-api-errors", "Enterprise API errors", "Every Enterprise API error code, what it means and what to do.",
         Html("\n".join(out)))


# ---- collections ----------------------------------------------------------------------

POSTMAN_SCHEMA = "https://schema.getpostman.com/json/collection/v2.1.0/collection.json"


def postman(api):
    folders = []
    for tag, desc, ops in api.tags():
        items = []
        for method, path, op in ops:
            item = api.spec["paths"][path]
            params = params_of(api, item, op)
            segs = [(":" + s[1:-1]) if s.startswith("{") else s for s in path.strip("/").split("/")]
            query = [{"key": p["name"], "value": param_value(p) or "", "disabled": param_value(p) is None,
                      "description": p.get("description", "")}
                     for p in params if p.get("in") == "query"]
            variables = [{"key": p["name"], "value": param_value(p) or "", "description": p.get("description", "")}
                         for p in params if p.get("in") == "path"]
            raw = "{{baseUrl}}/" + "/".join(segs)
            enabled = [q for q in query if not q["disabled"]]
            if enabled:
                raw += "?" + "&".join(f'{q["key"]}={q["value"]}' for q in enabled)
            url = {"raw": raw, "host": ["{{baseUrl}}"], "path": segs}
            if query:
                url["query"] = query
            if variables:
                url["variable"] = variables
            headers = []
            if api.is_org and method in WRITES:
                headers.append({"key": "Idempotency-Key", "value": "{{$guid}}"})
            request = {"method": method.upper(), "header": headers, "url": url,
                       "description": op.get("description", op.get("summary", ""))}
            ctype, example = json_body(api, op)
            if ctype == "application/json":
                headers.append({"key": "Content-Type", "value": "application/json"})
                request["body"] = {"mode": "raw", "raw": pretty(example),
                                   "options": {"raw": {"language": "json"}}}
            elif ctype:
                headers.append({"key": "Content-Type", "value": ctype})
                request["body"] = {"mode": "file", "file": {"src": ""}}
            if not needs_auth(op, api.spec):
                request["auth"] = {"type": "noauth"}
            items.append({"name": op.get("summary") or f"{method.upper()} {path}", "request": request})
        folders.append({"name": tag, "description": desc, "item": items})
    collection = {
        "info": {"name": api.spec["info"]["title"], "description": api.spec["info"].get("summary", ""),
                 "version": api.spec["info"]["version"], "schema": POSTMAN_SCHEMA},
        "auth": {"type": "bearer", "bearer": [{"key": "token", "value": "{{token}}", "type": "string"}]},
        "variable": [{"key": "baseUrl", "value": api.base}, {"key": "token", "value": ""}],
        "item": folders,
    }
    return pretty(collection) + "\n"


def bru_block(name, pairs):
    if not pairs:
        return ""
    lines = [f"{name} {{"] + [f"  {k}: {v}" for k, v in pairs] + ["}", ""]
    return "\n".join(lines)


def bruno(api, root):
    """One directory per spec: bruno.json, an environment, a folder per tag, a .bru per operation."""
    files = {
        "bruno.json": pretty({"version": "1", "name": api.spec["info"]["title"], "type": "collection",
                              "ignore": ["node_modules", ".git"]}) + "\n",
        "environments/production.bru": f"vars {{\n  baseUrl: {api.base}\n}}\nvars:secret [\n  token\n]\n",
    }
    for seq_tag, (tag, _, ops) in enumerate(api.tags(), 1):
        folder = anchor(tag)
        files[f"{folder}/folder.bru"] = f"meta {{\n  name: {tag}\n  seq: {seq_tag}\n}}\n"
        for seq, (method, path, op) in enumerate(ops, 1):
            item = api.spec["paths"][path]
            params = params_of(api, item, op)
            url = "{{baseUrl}}" + re.sub(r"\{([^}]+)\}", r":\1", path)
            query = [(p["name"], param_value(p)) for p in params
                     if p.get("in") == "query" and param_value(p) is not None]
            if query:
                url += "?" + "&".join(f"{k}={v}" for k, v in query)
            ctype, example = json_body(api, op)
            body_kind = "json" if ctype == "application/json" else "none"
            auth = "bearer" if needs_auth(op, api.spec) else "none"
            parts = [f"meta {{\n  name: {op.get('summary') or method.upper() + ' ' + path}\n  type: http\n  seq: {seq}\n}}\n",
                     f"{method} {{\n  url: {url}\n  body: {body_kind}\n  auth: {auth}\n}}\n"]
            block = bru_block("params:query", query)
            if block:
                parts.append(block)
            block = bru_block("params:path", [(p["name"], param_value(p) or "")
                                              for p in params if p.get("in") == "path"])
            if block:
                parts.append(block)
            headers = []
            if api.is_org and method in WRITES:
                headers.append(("~Idempotency-Key", "replace-with-a-unique-key"))
            if ctype and ctype != "application/json":
                headers.append(("Content-Type", ctype))
            block = bru_block("headers", headers)
            if block:
                parts.append(block)
            if auth == "bearer":
                parts.append("auth:bearer {\n  token: {{token}}\n}\n")
            if ctype == "application/json":
                body = "\n".join("  " + ln for ln in pretty(example).split("\n"))
                parts.append(f"body:json {{\n{body}\n}}\n")
            docs = op.get("description", "").strip()
            if docs:
                parts.append("docs {\n" + "\n".join(("  " + ln) if ln else "" for ln in docs.split("\n")) + "\n}\n")
            files[f"{folder}/{op_anchor(method, path, op)}.bru"] = "\n".join(parts)
    if root.exists():
        shutil.rmtree(root)
    for rel, text in sorted(files.items()):
        target = root / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text, encoding="utf-8")


ORG_INTRO = """
Every endpoint of the Enterprise API (`https://postbox.pigeonpost.dev/org/v1`), generated from
`openapi/pigeonpost-org.v1.yaml`. Read the [overview](/org-api) first for keys, scopes, pagination,
idempotency and rate limits; every error code is in the [error reference](/org-api-errors).

Downloads: [OpenAPI (JSON)](https://github.com/bekirdag/pigeonpost/blob/main/openapi/pigeonpost-org.v1.json),
[Postman collection](/collections/pigeonpost-org.postman_collection.json),
[Bruno collection](https://github.com/bekirdag/pigeonpost/tree/main/site-developers/collections/bruno/pigeonpost-org).
Both collections use the variables `{{baseUrl}}` and `{{token}}`; set `token` to your `pk_org_…` key.
"""

MEMBER_INTRO = """
Every endpoint of the postbox member API (`https://postbox.pigeonpost.dev`), generated from
`openapi/pigeonpost.v1.yaml`, one page per area. The [HTTP API](/api) page explains credentials and
the common flows; this reference lists every operation with its parameters, bodies and responses.
The internal service API (separate listener, service tokens) is not listed here.

Downloads: [OpenAPI (JSON)](https://github.com/bekirdag/pigeonpost/blob/main/openapi/pigeonpost.v1.json),
[Postman collection](/collections/pigeonpost.postman_collection.json),
[Bruno collection](https://github.com/bekirdag/pigeonpost/tree/main/site-developers/collections/bruno/pigeonpost).
Both collections use the variables `{{baseUrl}}` and `{{token}}`.
"""


def build_reference():
    for stale in sorted(OUT.glob("api-ref-*.html")) + sorted(OUT.glob("org-api-ref-*.html")):
        stale.unlink()
    collections = OUT / "collections"
    collections.mkdir(exist_ok=True)
    for api, intro in (
        (Api("pigeonpost-org.v1.json", "org-api-reference", "org-api-ref-", "Enterprise API",
             "PIGEONPOST_ORG_KEY", "pigeonpost-org"), ORG_INTRO),
        (Api("pigeonpost.v1.json", "api-reference", "api-ref-", "Member API",
             "PIGEONPOST_TOKEN", "pigeonpost"), MEMBER_INTRO),
    ):
        index_page(api, intro)
        for tag, desc, ops in api.tags():
            tag_page(api, tag, desc, ops)
        if api.is_org:
            errors_page(api)
        (collections / f"{api.collection}.postman_collection.json").write_text(postman(api), encoding="utf-8")
        bruno(api, collections / "bruno" / api.collection)


# ---- write -----------------------------------------------------------------------

def main():
    build_reference()
    for slug, (title, desc, body) in PAGES.items():
        page_html = SHELL.format(
            title=html.escape(title),
            desc=html.escape(desc),
            topnav=topnav(slug),
            sidebar=sidebar(slug),
            body=body if isinstance(body, Html) else render(body.strip()),
            gh=GH,
        )
        (OUT / f"{slug}.html").write_text(page_html, encoding="utf-8")
        print(f"  wrote {slug}.html")


if __name__ == "__main__":
    main()
