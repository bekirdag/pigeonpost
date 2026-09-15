#if DEBUG
import Foundation
import SwiftUI

/// Opt-in, in-memory scenarios for native iOS regressions. Never compiled into a release.
@MainActor
enum IOSUXFixtures {
    static func apply(account: Account, inbox: Inbox) {
        guard Fixtures.enabled else { return }
        if CommandLine.arguments.contains("-ios-mailboxes") {
            let rows = [
                Mailbox(address: "/k/ios-raw", handle: nil, label: "Scratch"),
                Mailbox(address: "/k/ios-sofya-child", handle: "/sofya/agent", label: nil),
                Mailbox(address: "/k/ios-sofya", handle: "/sofya/main", label: nil),
                Mailbox(address: "/k/ios-bekir-child", handle: "/bekir/work", label: nil),
                Mailbox(address: "/k/ios-lidya", handle: "/lidya/main", label: nil),
                Mailbox(address: "/k/ios-bekir", handle: "/bekir/main", label: nil)
            ]
            account.installFixtures(mailboxes: rows, me: rows[2])
        }
        if CommandLine.arguments.contains("-ios-history") {
            let now = Int(Date().timeIntervalSince1970)
            let rows = (0..<45).map { message(id: "history-\($0)", body: "History message \($0)", at: now - 100 + $0) }
            install(rows, inbox: inbox)
        }
    }

    static func receive(into inbox: Inbox) {
        let message = message(id: "history-arrival", body: "New arrival while reading history", at: Int(Date().timeIntervalSince1970))
        install(inbox.messages.filter { $0.id != message.id } + [message], inbox: inbox)
    }

    private static func install(_ messages: [Message], inbox: Inbox) {
        inbox.installFixtures(messages: messages, contacts: inbox.contacts, vocabulary: inbox.vocabulary, threads: inbox.serverThreads)
    }

    private static func message(id: String, body: String, at: Int) -> Message {
        let data = try! JSONSerialization.data(withJSONObject: [
            "message_id": id, "body": body, "direction": "in", "peer": "/bekir/agent1",
            "peer_handle": "/bekir/agent1", "thread_id": "t-agent1", "received_at": at, "read": true
        ])
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        return try! decoder.decode(Message.self, from: data)
    }
}

struct HistoryFixtureGrowth: View {
    @State private var expanded = false
    var body: some View {
        Color.clear.frame(height: expanded ? 600 : 1)
            .overlay(alignment: .bottom) { Text("Delayed content end").font(.caption) }
            .task {
                try? await Task.sleep(for: .seconds(2))
                guard !Task.isCancelled else { return }
                expanded = true
            }
    }
}
#endif
