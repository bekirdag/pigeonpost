import Foundation

/// Picker order is independent of the mailbox currently being read.
enum MailboxOrder {
    static func sorted(_ mailboxes: [Mailbox], primaryNamespace: String, username: String?) -> [Mailbox] {
        let rows = mailboxes.enumerated().map { index, mailbox in
            let path = (mailbox.handle ?? "").trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
            let parts = path.split(separator: "/")
            let named = path.hasPrefix("/") && !parts.isEmpty && parts.first != "k"
            let root = named && (parts.count == 1 || (parts.count == 2 && parts.last == "main"))
            return (index: index, mailbox: mailbox, path: path,
                    namespace: named ? String(parts[0]) : "", named: named, root: root)
        }
        func namespace(_ value: String) -> String {
            value.trimmingCharacters(in: CharacterSet(charactersIn: "/").union(.whitespacesAndNewlines)).lowercased()
        }
        let roots = rows.filter(\.root)
        let username = namespace(username ?? "")
        let primary = roots.first { !username.isEmpty && $0.namespace == username }
            ?? roots.first { $0.namespace == namespace(primaryNamespace) }
            ?? roots.first
        func rank(_ index: Int, _ named: Bool, _ root: Bool) -> Int {
            if index == primary?.index { return 0 }
            return root ? 1 : named ? 2 : 3
        }
        return rows.sorted { a, b in
            let ar = rank(a.index, a.named, a.root), br = rank(b.index, b.named, b.root)
            if ar != br { return ar < br }
            if a.namespace != b.namespace { return a.namespace < b.namespace }
            if a.path != b.path { return a.path < b.path }
            return a.index < b.index
        }.map(\.mailbox)
    }
}
