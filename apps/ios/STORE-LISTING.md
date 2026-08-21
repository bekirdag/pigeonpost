# App Store Connect — every field, and what goes in it

App: **Pigeonpost Inbox**, Apple ID `6803521541`, SKU `pigeonpost-ios`, bundle `dev.pigeonpost.inbox`.
Build **1.0 (1)** is already uploaded and `VALID`, in the `Pigeonpost Internal` TestFlight group.

Character counts below are measured, not estimated. Two of them are at the limit.

## App Information — left sidebar, General → App Information

| Field | Value |
| --- | --- |
| Name | `Pigeonpost Inbox` (16/30 — already set) |
| Subtitle | `Your agents' mail in one place` (**30/30 — no room to edit**) |
| Privacy Policy URL | `https://pigeonpost.dev/privacy` |
| Category, primary | **Developer Tools** |
| Category, secondary | **Productivity** |
| Content Rights | Contains no third-party content |
| Age Rating | see below — it is not simply 4+ |
| License Agreement | Apple's standard EULA |

### Age rating, answered honestly

The questionnaire has a question about user-generated content, and the answer is **yes**: this app
displays messages written by other people and their agents. Answering it truthfully is what obliges
the app to offer reporting and blocking, and both exist — say where when App Review asks:

- **Report spam** — long-press any incoming message.
- **Block this sender** — the ⓘ button in a conversation, then *Block this sender*.

Everything else in the questionnaire is *None*: no violence, no profanity, no gambling, no contests,
no medical or drug references. The result should come out **4+**.

## Pricing and Availability

| Field | Value |
| --- | --- |
| Price | **Free** |
| Availability | All territories |

## App Privacy — left sidebar

Privacy Policy URL: `https://pigeonpost.dev/privacy`

**Data collected — exactly two types.** Both are *linked to the user* and neither is used for
tracking. When asked "Do you or your third-party partners use data for tracking?" the answer is
**No** — there is no analytics, no advertising identifier, and no third-party code of any kind in
this app.

| Category | Type | Linked | Tracking | Purpose |
| --- | --- | --- | --- | --- |
| Identifiers | User ID | Yes | No | App Functionality |
| User Content | Other User Content | Yes | No | App Functionality |

Everything else — contact info, health, financial info, location, browsing history, search history,
usage data, diagnostics, purchases, contacts, sensitive info — is **not collected**.

That answer matches `Pigeonpost/PrivacyInfo.xcprivacy` in the build, which declares the same two
types plus one accessed API (`UserDefaults`, reason `CA92.1`). If you change one, change the other:
a listing that disagrees with the manifest is a rejection.

## The version page — 1.0, Prepare for Submission

| Field | Value |
| --- | --- |
| Promotional Text | see below (168/170) |
| Description | see below |
| Keywords | `agent,agents,inbox,mailbox,messaging,async,developer,devtools,cli,automation,threads` (84/100) |
| Support URL | `https://pigeonpost.dev` |
| Marketing URL | `https://pigeonpost.dev` |
| Version | `1.0` |
| Copyright | `2026 <the legal entity exactly as it appears on the developer account>` |
| Build | select **1.0 (1)** — already uploaded |
| Screenshots, 6.9" | `apps/ios/Screenshots/6.9-inch/` in this repo |
| App Preview | none |
| Version Release | **Manually release this version** |

No keyword may be a trademark you do not own — no model names, no company names. The list above is
deliberately generic for that reason.

### Promotional Text — 168/170

```
Read and answer your agents' mail from your phone — the same inbox as inbox.pigeonpost.dev, with the threads, the held requests and the trust decisions the server made.
```

### Description

```
Pigeonpost gives every AI agent a permanent address and a private inbox. This is the app for reading that mail.

Your agents write while you are away — a build went green, a deploy needs a decision, a question is waiting on an answer only you have. Pigeonpost holds those messages until somebody opens them. This is where you read and answer them.

WHAT IT DOES

• Both halves of every conversation. What an agent sent you and what you sent back, whether you answered from this app, from the browser, or from the command line.
• Threads. One subject kept apart from the rest, so last week's question does not colour today's.
• The decisions the server made. When an agent asks for something — run the tests, report status — the server decides whether that request may be acted on, and says why not when it declines. The app shows that decision. It never makes one.
• Your whole fleet. If you run several agents, each has its own mailbox; switch between them and read as any of them.
• Who gets in. Admission, autonomy, and exactly which requests a sender may have acted on — per sender, or for a whole namespace at once.
• Archive, search, and unread counts, the way a messenger does them.

WHAT IT DOES NOT DO

• It does not act on messages by itself. Nothing in a message body is executed, followed, or opened by this app. Another agent's words are shown as text and nothing more.
• It does not track you. No analytics, no advertising identifiers, no third-party code of any kind.

YOU NEED A PIGEONPOST ACCOUNT

Pigeonpost is free and open source. This app signs in to the hosted postbox and shows the mailboxes your account owns. If you do not have an account yet, start at pigeonpost.dev.

Open source: github.com/bekirdag/pigeonpost
```

### Screenshots

Two are in the repository at `apps/ios/Screenshots/6.9-inch/`, 1320×2868, taken from the app's
fixture mailbox so nothing real or private is on a store page:

1. `store-1-list.png` — the conversation list, with an unread stranger and a held request.
2. `store-2-thread.png` — a conversation with two subjects and a scoped request shown as what it
   asks for.

A third is worth adding and needs two taps rather than a launch flag: **Settings → a sender under
Trusted senders**, which is the screen that makes the product's argument about admission and
autonomy. `Screenshots/README.md` has the exact commands.

Upload them under **6.9" Display**. iPad shots are not needed: the app ships iPhone-only
(`TARGETED_DEVICE_FAMILY = 1`).

## App Review Information — the part that gets apps rejected

**Sign-In Required: Yes.** A reviewer who cannot get past the first screen rejects the app, so this
needs a real account that still works on the day they look.

| Field | Value |
| --- | --- |
| Username | *the demo account — see below, it does not exist yet* |
| Password | *the demo account* |
| Contact | first name, last name, phone number, and a contact address for whoever answers |

### Notes — paste this, with the account added

```
Pigeonpost is free, open messaging infrastructure that gives an AI agent a permanent address and a private inbox (pigeonpost.dev, open source at github.com/bekirdag/pigeonpost). This app reads that mail.

Sign in with the demo account above. It opens on a mailbox with several conversations already in it, including one message that is a scoped request the server is holding for a decision — that is the "held" state the description refers to.

Message content is written by other accounts and their agents. The app renders every message body as plain text: nothing in a message is executed, followed, or turned into a link.

Reporting and blocking, since the app receives content from other users:
• Report spam — press and hold any incoming message, then "Report spam".
• Block this sender — open a conversation, tap the ⓘ button, then "Block this sender".

The app does not use push notifications in this version.
```

### The demo account does not exist yet

It needs a user in the `pigeonpost-prod` Keycloak realm, which is owned by another team, plus a
postbox mailbox seeded with a few conversations and at least one held request. Ask for it before you
need it; everything else on this page can be filled in while you wait.

## TestFlight tab — needed only for external testers

| Field | Value |
| --- | --- |
| Beta App Description | the first two paragraphs of the Description above |
| Feedback address | whoever answers |
| Marketing URL | `https://pigeonpost.dev` |
| Privacy Policy URL | `https://pigeonpost.dev/privacy` |
| Sign-in required | Yes — the same demo account |

Internal testing needs none of this. Build 1.0 (1) is already in the `Pigeonpost Internal` group.

## Do not change

- **The bundle id.** `dev.pigeonpost.inbox` is what the OIDC callback
  `dev.pigeonpost.inbox://oauth2redirect` is registered against in the identity realm, which is a
  different team's repository.
- **Export compliance.** Already answered by the build itself: `ITSAppUsesNonExemptEncryption` is
  `false` in `Info.plist`, which is why the upload never asked. It uses HTTPS and the platform's own
  cryptography, nothing more.
- **Push notifications.** Not enabled, deliberately — the server cannot deliver them yet, and an app
  that asks for permission and then never rings is worse than one that never asks.

## One thing to check before submitting

Open `auth.pigeonpost.dev` and look at the sign-in page. **If it offers third-party sign-in — Google,
GitHub, Apple — guideline 4.8 requires Sign in with Apple to be offered alongside it.** If it is only
a username and password, 4.8 does not apply and there is nothing to do. Either way, find out before
the submission rather than during it.
