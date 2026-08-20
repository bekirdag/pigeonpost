# App Store screenshots

`6.9-inch/` — 1320×2868, taken on an iPhone 16 Pro Max simulator. That is the size Apple asks for
first; the others are derived from it or optional.

**Taken from the fixture mailbox, never a real one.** Everything on screen is invented in
`Pigeonpost/Debug/Fixtures.swift`: the agents, the addresses, the messages, the held `run_tests`
request. Nothing private, half-finished, or belonging to a real correspondent ends up on a store
page.

```
xcrun simctl boot "iPhone 16 Pro Max"
xcrun simctl install "iPhone 16 Pro Max" <path to Pigeonpost.app>
xcrun simctl launch "iPhone 16 Pro Max" dev.pigeonpost.inbox -fixtures
xcrun simctl io "iPhone 16 Pro Max" screenshot 6.9-inch/store-1-list.png
xcrun simctl terminate "iPhone 16 Pro Max" dev.pigeonpost.inbox
xcrun simctl launch "iPhone 16 Pro Max" dev.pigeonpost.inbox -fixtures "-open=/bekir/agent1"
xcrun simctl io "iPhone 16 Pro Max" screenshot 6.9-inch/store-2-thread.png
```

1. **store-1-list** — the conversation list: an agent's fleet, an unread stranger, a held request.
2. **store-2-thread** — a conversation with two subjects, both halves of it, and a scoped request
   rendered as what it asks for with the reason it was held.

A third worth having is the trusted-sender editor, which is what makes the product's argument about
admission and autonomy. It needs two taps to reach, so it is not scriptable through launch arguments
the way these two are — take it by hand: Settings → a sender under Trusted senders.
