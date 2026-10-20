//  Mail the postbox filed under Filtered (SDS §13.4): strangers, unnamed addresses and reported
//  senders, depending on the mailbox's policy. None of it rang, none of it is unread, and none of it
//  is in the ordinary listing — so it lives in its own row at the foot of the conversation list.

import Foundation
import Observation

/// One sender's filtered mail.
struct FilteredSender: Identifiable, Equatable {
    let peer: String
    var messages: [Message]
    var last: Int
    var id: String { peer }
    /// The address their latest message came from, which is what `accept` names.
    var from: String { messages.last?.from ?? peer }
}

@MainActor
@Observable
final class FilteredMail {
    let account: Account
    private(set) var messages: [Message] = []
    var toast: String?
    @ObservationIgnored private var mailbox: String?

    init(account: Account) {
        self.account = account
    }

    var enabled: Bool { account.features.filtering }

    /// Grouped by sender, newest sender first, without anybody already blocked.
    func senders(contacts: [Contact]) -> [FilteredSender] {
        guard enabled else { return [] }
        return Self.group(messages, contacts: contacts)
    }

    nonisolated static func group(_ messages: [Message], contacts: [Contact]) -> [FilteredSender] {
        var order: [String] = []
        var byPeer: [String: FilteredSender] = [:]
        for message in messages where !message.isOutgoing {
            let key = message.peerKey
            if ConversationBuilder.contact(for: key, in: contacts)?.admission == "block" { continue }
            if byPeer[key] == nil {
                byPeer[key] = FilteredSender(peer: key, messages: [], last: 0)
                order.append(key)
            }
            byPeer[key]?.messages.append(message)
            let last = max(byPeer[key]?.last ?? 0, message.at)
            byPeer[key]?.last = last
        }
        return order.compactMap { key in
            guard var sender = byPeer[key] else { return nil }
            sender.messages.sort { $0.at < $1.at }
            return sender
        }.sorted { $0.last > $1.last }
    }

    func refresh() async {
        let current = account.me?.address
        if current != mailbox { mailbox = current; messages = [] }
        guard let address = current, enabled, !Fixtures.enabled else { return }
        do {
            let got = try await account.client.filteredInbox(identity: address)
            guard address == account.me?.address else { return }
            messages = got.filter { !$0.isOutgoing }
        } catch let error as APIError where [400, 404, 501].contains(error.status) {
            messages = []
        } catch {
            // Keep what was there.
        }
    }

    /// Let this sender in: an allow row by name, and their mail moves to the inbox.
    @discardableResult
    func accept(_ sender: FilteredSender, inbox: Inbox) async -> Bool {
        guard let address = account.me?.address else { return false }
        do {
            if !Fixtures.enabled {
                try await account.client.acceptFiltered(identity: address, peer: sender.from)
            }
            messages.removeAll { $0.peerKey == sender.peer }
            toast = "Accepted. Their messages are in your inbox now."
            if !Fixtures.enabled { await inbox.loadAll() }
            return true
        } catch {
            toast = "Could not accept. Please try again."
            return false
        }
    }

    /// Forget mail from a sender just blocked; the server stops showing it too.
    func drop(peer: String) {
        messages.removeAll { $0.peerKey == peer }
    }

    #if DEBUG
    func installFixture() {
        mailbox = account.me?.address
        let now = Int(Date().timeIntervalSince1970)
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        let json = """
        {"messages":[
         {"message_id":"f1","from":"/k/spam0000spam0000spam0000sp","peer":"/k/spam0000spam0000spam0000sp",
          "direction":"in","body":"Congratulations, your agent has won a free GPU cluster. Reply to claim.",
          "received_at":\(now - 3000),"read":false,"filtered":true},
         {"message_id":"f2","from":"/k/spam0000spam0000spam0000sp","peer":"/k/spam0000spam0000spam0000sp",
          "direction":"in","body":"Last chance — the offer ends today.","received_at":\(now - 1500),"read":false,"filtered":true},
         {"message_id":"f3","from":"/k/new00000new00000new00000ne","peer":"/olga/main","peer_handle":"/olga/main",
          "sender_handle":"/olga/main","direction":"in","body":"Hi — Ana gave me this address. Can we talk about the deploy?",
          "received_at":\(now - 600),"read":false,"filtered":true}
        ]}
        """
        messages = (try? decoder.decode(InboxResponse.self, from: Data(json.utf8)))?.messages ?? []
    }
    #endif
}
