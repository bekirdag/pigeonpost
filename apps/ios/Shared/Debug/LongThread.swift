//  A conversation long enough, and with rows tall enough, to catch the scroll bug.
//
//  `-fixtures` on its own is six short messages, and six short messages fit on one screen: a thread
//  that opens anywhere at all opens at its end, so the fixture mailbox has never been able to show
//  the defect this exists for. `-fixtures -long` adds ninety messages of the kind these agents
//  actually send to the `/bekir/agent1` conversation, and `-long=400` adds four hundred.
//
//  Two things are deliberate about the shape, and the second one is the one that matters.
//
//  The lengths are uneven, because what breaks the placement is a `LazyVStack` guessing a row's
//  height and being wrong, and rows that were all the same size would hide the bug as surely as six
//  of them do.
//
//  And some of the rows are enormous — several screens each, with the last message in the thread
//  always one of them. That is what the screen recording showed: not a thread of many small
//  messages, but a thread whose final answer is a four-screen report, opening two screens short of
//  its own end. A fixture of ninety one-screen rows lands correctly every time and proves nothing.
//
//  Debug only, and inert unless asked for by name.

import Foundation

enum LongThread {
    static var enabled: Bool {
        #if DEBUG
        // Not on `-empty`, which is a brand-new account owning nothing. History in a mailbox that
        // is meant to have none would be testing a state that cannot exist.
        return Fixtures.enabled && !Fixtures.emptyAccount && CommandLine.arguments.contains("-long")
        #else
        return false
        #endif
    }

    /// How many to add. `-long=400` for a thread that is longer still.
    static var count: Int {
        let arg = CommandLine.arguments.first { $0.hasPrefix("-long=") }
        return arg.flatMap { Int($0.dropFirst("-long=".count)) } ?? 90
    }

    /// The last line of the last message, and of nothing else. A test that wants to know whether the
    /// thread opened at its end asks whether this is on screen.
    static let lastLine = "Nothing else is outstanding."

    #if DEBUG
    /// Fold the long thread into whatever `Fixtures` already installed, keeping its contacts and its
    /// thread list. Called after `Fixtures.apply`, so the mailbox is the same one in either mode and
    /// only the amount of history differs.
    @MainActor
    static func install(into inbox: Inbox) {
        guard enabled else { return }
        let now = Int(Date().timeIntervalSince1970)
        inbox.installFixtures(
            messages: inbox.messages + messages(now: now),
            contacts: inbox.contacts,
            vocabulary: inbox.vocabulary,
            threads: inbox.serverThreads
        )
    }
    #endif

    /// Spread over a fixed span rather than at a fixed interval, so `-long=400` covers the same
    /// stretch of history as `-long=90` and ends in the same place. Asking for more history must not
    /// move where the thread ends, or a longer run would be testing whether a different message is
    /// on screen, which is not the question.
    static func messages(now: Int) -> [Message] {
        let span = 200_000
        let step = max(span / max(count, 1), 1)
        let rows: [[String: Any]] = (0..<count).map { index in
            // Newest last, and the newest of all of them is the last thing in the conversation.
            let at = now - span + (index + 1) * step - 60
            let outgoing = index % 3 == 2 && index != count - 1
            var row: [String: Any] = [
                "message_id": "long-\(index)",
                "body": body(index),
                "peer": "/bekir/agent1",
                "peer_handle": "/bekir/agent1",
                "thread_id": "t-agent1",
                "read": true,
            ]
            if outgoing {
                row["direction"] = "out"
                row["from"] = "/k/cz6900v2h90vnwefj7g7ezvbh4"
                row["to"] = "/bekir/agent1"
                row["sent_at"] = at
                row["received_at"] = at
            } else {
                row["direction"] = "in"
                row["from"] = "/k/aaaa1111bbbb2222cccc3333dd"
                row["sender_handle"] = "/bekir/agent1"
                row["sender_standing"] = "unproven"
                row["sender_tier"] = "handle"
                row["sender_known"] = true
                row["matched_contact"] = "/bekir/*"
                row["autonomy"] = "auto"
                row["received_at"] = at
            }
            return row
        }
        guard let data = try? JSONSerialization.data(withJSONObject: rows) else { return [] }
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return (try? decoder.decode([Message].self, from: data)) ?? []
    }

    /// Shapes, cycled, each padded by a different amount: a two-line answer, a bulleted list, a
    /// fenced block that does not wrap, a table that does not either — and, every eleventh message
    /// and always the last one, the multi-screen report that is what actually goes through these
    /// mailboxes.
    private static func body(_ index: Int) -> String {
        let n = index + 1
        if index == count - 1 || index % 11 == 10 { return report(n, last: index == count - 1) }
        switch index % 5 {
        case 0:
            return "Done — \(n) of the pipeline's stages are green."
        case 1:
            return """
                ## Round \(n)

                Ran the suite against the working tree and then against `origin/main`, because the \
                two disagreed about one case and I wanted to know which of them was wrong.

                - `pnpm typecheck` — clean
                - `pnpm test` — 87/87
                - `cargo fmt --check` — clean
                - `cargo clippy --all-targets -- -D warnings` — clean

                The disagreement was mine: a stale build artefact in `target/debug`. Removed it and \
                the two agree again.
                """
        case 2:
            return """
                Reading it back, the ordering matters more than I said. Step \(n) writes the row \
                that step \(n + 1) reads, so running them the other way round does not fail — it \
                succeeds against yesterday's row, which is worse.
                """
        case 3:
            return """
                ### What the review turned up

                ```ts
                const key = tenant.gateway?.stripe?.secret ?? process.env.STRIPE_API_KEY
                if (!tenant.gateway?.stripe) throw new PaymentSetupIncomplete(tenant.id)
                ```

                | Check | Runner | Time | Result |
                | --- | --- | ---: | :---: |
                | fmt | ubuntu-24.04 | 4s | pass |
                | clippy | ubuntu-24.04 | 51s | pass |
                | test (linux) | ubuntu-24.04 | 2m18s | pass |
                """
        default:
            return "Watched it for a minute. Still \(n) arrivals an hour and none of them linked."
        }
    }

    /// Four screens of one message. The recording's last message was this shape, and the thread
    /// stopped two screens above the end of it.
    private static func report(_ n: Int, last: Bool) -> String {
        let sections = (1...6).map { part in
            """
            ### \(part). What round \(n) found

            The automation branch is clean, pushed, and merged into `main`; the working tree is \
            clean and origin-aligned. The plan still has \(44 - part) unfinished items, including \
            replay safety, queue prioritisation and reservation, checkpointing, throughput SLOs, \
            and golden tests. None of them are blocked on anything I can reach from here.

            - Round \(n * part) finished one generation with no champion; best fitness was 0.2125.
            - Round \(n * part + 1) produced authoring prompts but never reached evaluation.
            - The latest throughput audit found \(n * 137) arrivals an hour with zero stories \
            linked, and \(n * 4_213) items older than an hour still untouched.
            - The five-story quality review found \(part) issues and no reviewer-infrastructure \
            failures, which is the part that surprised me — the reviewers are fine, they are \
            simply never reached.

            ```sh
            pnpm --filter @wodo/pipeline test -- --reporter=verbose --run
            cargo test -p pigeonpost-postbox -- --nocapture stage_\(part)
            ```

            | Stage | Runner | Time | Result |
            | --- | --- | ---: | :---: |
            | fmt | ubuntu-24.04 | 4s | pass |
            | clippy | ubuntu-24.04 | 51s | pass |
            | test (linux) | ubuntu-24.04 | 2m18s | pass |
            | test (macos) | macos-15 | 3m02s | pass |

            Reading that back, the ordering matters more than I said above: stage \(part) writes \
            the row stage \(part + 1) reads, so running them the other way round does not fail — \
            it succeeds against yesterday's row, which is a great deal worse than failing.
            """
        }
        let tail = last
            ? "\n\nThis was a read-only check and I changed no repository or production state. \(lastLine)"
            : "\n\nThis was a read-only check. I changed nothing."
        return "## Status, round \(n)\n\n" + sections.joined(separator: "\n\n") + tail
    }
}
