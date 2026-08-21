//  A mailbox with mail in it, without a network.
//
//  Launch with `-fixtures` and the app loads the same response shapes the thread-model test uses
//  (and the web app's tests before that) instead of signing in. It exists because the two things
//  that gate a real session — a realm client and a postbox account — are not always to hand, and
//  because a screenshot of an empty list proves nothing about the screens that matter.
//
//  Debug only, and inert unless asked for by name: the flag is not something a shipped build can be
//  talked into by a URL or a setting.

import Foundation

enum Fixtures {
    static let enabled: Bool = {
        #if DEBUG
        return CommandLine.arguments.contains("-fixtures")
        #else
        return false
        #endif
    }()

    /// `-open=/bekir/agent1` opens straight into that conversation, so the thread can be looked at
    /// without a tap — which is what makes a screenshot of it reproducible.
    ///
    /// One token, not `-open <peer>`: a bare `-key value` pair on the command line is read by
    /// UserDefaults as a default, and passing two of them was enough to stop `-fixtures` being seen
    /// at all.
    static var openPeer: String? {
        guard enabled else { return nil }
        return CommandLine.arguments
            .first { $0.hasPrefix("-open=") }
            .map { String($0.dropFirst("-open=".count)) }
    }

    /// `-sheet=settings` opens a sheet straight away, so the screens that need two taps to reach can
    /// be looked at — and screenshotted — without any. `settings`, `new`, `identities`, or `peer`
    /// (which needs `-open=` as well, since a sender panel is about somebody).
    static var sheet: String? {
        guard enabled else { return nil }
        return CommandLine.arguments
            .first { $0.hasPrefix("-sheet=") }
            .map { String($0.dropFirst("-sheet=".count)) }
    }

    /// `-handle=sale` or `-handle=owned` puts the buy-a-handle section into a fixed state.
    ///
    /// Apple requires a screenshot of the purchase screen before the subscription can be reviewed,
    /// and the purchase screen cannot be reached without a live product — which cannot exist until
    /// the subscription is reviewed. This is the way out of that circle, and it is honest: the
    /// section is the real one, with the real copy, showing what a real storefront would show.
    static var handleState: String? {
        guard enabled else { return nil }
        return CommandLine.arguments
            .first { $0.hasPrefix("-handle=") }
            .map { String($0.dropFirst("-handle=".count)) }
    }

    /// `-empty` is a brand-new account: signed in, owning nothing. The one state a real account can
    /// only be in once, and the one that used to leave the app on a spinner for ever.
    static var emptyAccount: Bool {
        enabled && CommandLine.arguments.contains("-empty")
    }

    /// `-quota=near` or `-quota=full` stages the mailbox-usage section.
    static var quotaState: String? {
        guard enabled else { return nil }
        return CommandLine.arguments
            .first { $0.hasPrefix("-quota=") }
            .map { String($0.dropFirst("-quota=".count)) }
    }

    #if DEBUG
    @MainActor
    static func apply(session: Session, account: Account, inbox: Inbox) {
        let now = Int(Date().timeIntervalSince1970)
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase

        if emptyAccount {
            session.installFixtureSession()
            account.installEmptyFixture()
            return
        }

        let me = Mailbox(address: "/k/cz6900v2h90vnwefj7g7ezvbh4", handle: "/bekir/main", label: "main")
        let fleet = [
            me,
            Mailbox(address: "/k/zz1111v2h90vnwefj7g7ezvbh9", handle: "/bekir/docdex", label: "docdex box"),
            Mailbox(address: "/k/qq2222v2h90vnwefj7g7ezvbh7", handle: nil, label: "scratch"),
        ]

        let messages = (try? decoder.decode(InboxResponse.self, from: Data(inboxJSON(now).utf8)))?.messages ?? []
        let contactsBody = try? decoder.decode(ContactsResponse.self, from: Data(contactsJSON.utf8))
        let threads = (try? decoder.decode(ThreadsResponse.self, from: Data(threadsJSON(now).utf8)))?.threads ?? []

        session.installFixtureSession()
        account.installFixtures(mailboxes: fleet, me: me)
        inbox.installFixtures(
            messages: messages,
            contacts: contactsBody?.contacts ?? [],
            vocabulary: contactsBody?.vocabulary,
            threads: threads
        )
    }

    private static func inboxJSON(_ now: Int) -> String {
        """
        {"messages":[
         {"message_id":"m1","from":"/k/aaaa1111bbbb2222cccc3333dd","body":"the build is green — 0.5.21 tagged and the npm job went through unattended",
          "sender_standing":"unproven","sender_tier":"handle","sender_known":true,"matched_contact":"/bekir/*",
          "sender_handle":"/bekir/agent1","peer":"/bekir/agent1","peer_handle":"/bekir/agent1",
          "thread_id":"t-agent1","direction":"in","autonomy":"review","held_because":"not_a_request",
          "received_at":\(now - 7200),"read":true},

         {"message_id":"m_out1","direction":"out","from":"/k/cz6900v2h90vnwefj7g7ezvbh4","to":"/bekir/agent1",
          "peer":"/bekir/agent1","peer_handle":"/bekir/agent1","thread_id":"t-agent1",
          "body":"good. run the unit suite once more before the release gate","sent_at":\(now - 3600),
          "received_at":\(now - 3600),"read":true},

         {"message_id":"m2","from":"/k/aaaa1111bbbb2222cccc3333dd",
          "body":"{\\"v\\":1,\\"verb\\":\\"run_tests\\",\\"args\\":{\\"suite\\":\\"unit\\",\\"target\\":\\"crates/pigeonpost-postbox\\"},\\"note\\":\\"before the tag\\"}",
          "sender_standing":"unproven","sender_tier":"handle","sender_known":true,"matched_contact":"/bekir/*",
          "sender_handle":"/bekir/agent1","peer":"/bekir/agent1","peer_handle":"/bekir/agent1",
          "thread_id":"t-agent1","direction":"in","autonomy":"review","verb":"run_tests",
          "held_because":"verb_denied","received_at":\(now - 900),"read":false},

         {"message_id":"m_out2","direction":"out","from":"/k/cz6900v2h90vnwefj7g7ezvbh4","to":"/bekir/agent1",
          "peer":"/bekir/agent1","peer_handle":"/bekir/agent1","thread_id":"t-agent1",
          "body":"{\\"v\\":1,\\"verb\\":\\"make_change\\",\\"args\\":{\\"task\\":\\"pin the flaky macOS test and push a fix\\"},\\"note\\":\\"pin the flaky macOS test and push a fix\\"}",
          "sent_at":\(now - 600),"received_at":\(now - 600),"read":true},

         {"message_id":"m_reply","from":"/k/aaaa1111bbbb2222cccc3333dd",
          "body":"pigeonpost-auto-reply v1 in_reply_to=m_out2 answered=make_change\\nGenerated unattended by this mailbox's agent. Nobody read it before it was sent.\\n\\nPinned it — the fixture listener was non-blocking, which macOS inherits onto the accepted socket and Linux does not. set_nonblocking(false) in serve_http_request, pushed as 4f21c0e.",
          "sender_standing":"unproven","sender_tier":"handle","sender_known":true,"matched_contact":"/bekir/*",
          "sender_handle":"/bekir/agent1","peer":"/bekir/agent1","peer_handle":"/bekir/agent1",
          "thread_id":"t-agent1","direction":"in","autonomy":"auto","received_at":\(now - 420),"read":true},

         {"message_id":"m_docdex","from":"/k/zz1111v2h90vnwefj7g7ezvbh9","body":"index rebuilt — 41k symbols, 1.2s",
          "sender_standing":"unproven","sender_tier":"handle","sender_known":true,"matched_contact":"/bekir/*",
          "sender_handle":"/bekir/docdex","peer":"/bekir/docdex","peer_handle":"/bekir/docdex",
          "direction":"in","autonomy":"auto","received_at":\(now - 108000),"read":true},

         {"message_id":"m3","from":"/k/eeee5555ffff6666gggg7777hh","body":"hello — are you the maintainer of pigeonpost?",
          "sender_standing":"unproven","sender_tier":"anonymous","sender_known":false,
          "peer":"/k/eeee5555ffff6666gggg7777hh","thread_id":"t-stranger","direction":"in",
          "autonomy":"review","held_because":"sender_not_auto","received_at":\(now - 240),"read":false}
        ],"policy":{"accept_all":true,"auto_accept_known":false}}
        """
    }

    private static let contactsJSON = """
    {"contacts":[{"peer":"/bekir/*","alias":"my fleet","admission":"allow","autonomy":"review","allowed_verbs":[]}],
     "policy":{"accept_all":true,"auto_accept_known":false},
     "vocabulary":{"grantable":["report_status","answer_question","read_file","run_tests"],
     "never_auto":["git_push","deploy","read_credentials","spend","delete_files","run_shell"]}}
    """

    private static func threadsJSON(_ now: Int) -> String {
        """
        {"threads":[
         {"thread_id":"t-agent1","peer":"/bekir/agent1","title":null,"is_default":true,
          "created_at":\(now - 9000),"last_at":\(now - 900),"archived":false},
         {"thread_id":"t-agent1-deploy","peer":"/bekir/agent1","title":"the deploy","is_default":false,
          "created_at":\(now - 8000),"last_at":\(now - 8000),"archived":false},
         {"thread_id":"t-stranger","peer":"/k/eeee5555ffff6666gggg7777hh","title":null,"is_default":true,
          "created_at":\(now - 240),"last_at":\(now - 240),"archived":false}]}
        """
    }
    #endif
}
