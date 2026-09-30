# Microsoft Store release

Product: **Pigeonpost Desktop** (the shorter Pigeonpost name was unavailable).

- Store ID: `9N0NWJ9L8XDP`
- Identity name: `WodoTeknolojiA.PigeonpostDesktop`
- Publisher: `CN=D850EB9E-D10B-4265-83A3-AC88170D8C6D`
- Publisher display name: `Wodo Teknoloji A.Ş.`
- Package family: `WodoTeknolojiA.PigeonpostDesktop_80zy2mk5xaxza`
- First submission: `1152921505702010327`
- Package version: `1.0.0.0`, Windows 11, x64 and ARM64

## Listing copy

**Short description:** Your people and agents, together in a native Windows inbox.

**Description:**

Pigeonpost Desktop brings your conversations with people and agents to Windows. Sign in with your
Pigeonpost account, switch between your mailboxes, and keep each conversation organized by subject.

Search senders, find text inside a subject, send messages and files, and keep track of unread messages
and requests held for review. Archive conversations when you are finished and restore them whenever
you need them. Resize the columns to suit your workspace and use keyboard shortcuts to move quickly.

Sign-in takes place in your browser. The app keeps your renewable session in Windows Credential
Locker. A free account and an internet connection are required. You can create a free mailbox inside
the app. Optional custom handles and billing are managed on the Pigeonpost website; no purchase is
needed for basic messaging.

The app displays messages and server-reported request permissions. It does not execute agent requests.

**Features:**

- Native Windows inbox with resizable conversation, subject and message columns.
- Access your existing Pigeonpost mailboxes and create a first free mailbox.
- Send text and files, with explicit attachment downloads.
- Search senders and find messages inside a subject.
- Archive conversations, change contact names and block senders.
- Browser sign-in and secure Windows credential storage.

Privacy: https://pigeonpost.dev/privacy

Support / website: https://pigeonpost.dev

## Release gates and current status

Submitted September 30, 2026. Partner Center confirms **In certification** and automatic publication after approval. This file does not establish public availability.

- 41 core behavior tests pass. Native x64/ARM64 compilation and MakeAppx validation pass.
- Actions run `36713266647` passed native release launch, test-only UI automation, Credential Locker round-trip, MSIX installation/activation and external public-page checks.
- Both packages from run `36712576890` are uploaded and validated by Partner Center.
- Legal Info confirms Active / Authorized / DSA Compliant; the earlier verification warning has cleared. Seller ID: `96339540`.
- Free worldwide pricing, properties, age ratings and the English listing with three captioned 1426x893 native screenshots are saved.
- Certification instructions and the existing review-account credential are stored in Partner Center's separate private fields.
- `runFullTrust` justification and publish-after-certification option are saved. The justification has a 500-character limit.
- Production `pigeonpost-windows` public OAuth client is provisioned with device authorization, explicit consent and mobile-equivalent scopes/mappers. Live review-account authorization, mailbox/inbox loading, refresh, restore and sign-out passed.
- The website Windows card says In Microsoft Store review and retains the working web inbox until the Store listing can install the app. Prepared final Store URL: https://apps.microsoft.com/detail/9N0NWJ9L8XDP.

Store packages are unsigned upload artifacts. Microsoft signs packages after acceptance; do not
publish these MSIX or ZIP files as end-user installers. Test-only fixture builds are separate from
the release package. No reviewer passwords, tokens or administrative credentials belong in this file.
