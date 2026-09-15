import Foundation

@main
enum IOSNavigationTests {
    static func main() {
        func box(_ handle: String?, _ address: String) -> Mailbox {
            Mailbox(address: address, handle: handle, label: nil)
        }
        let fleet = [box(nil, "/k/anonymous"), box("/sofya/agent", "s1"), box("/sofya/main", "s0"),
                     box("/bekir/work", "b1"), box("/lidya/main", "l0"), box("/bekir/main", "b0"),
                     box("/k/legacy", "k1")]
        let sorted = MailboxOrder.sorted(fleet, primaryNamespace: "/bekir", username: nil)
        precondition(sorted.map(\.address) == ["b0", "l0", "s0", "b1", "s1", "/k/anonymous", "k1"])
        precondition(Set(sorted) == Set(fleet), "The picker must preserve every owned mailbox")
        precondition(MailboxOrder.sorted(fleet, primaryNamespace: "/bekir", username: "/SOFYA/").first?.address == "s0")
        let otherUser = [box("/alp/main", "a"), box("/bought/main", "b"), box(nil, "k")]
        precondition(MailboxOrder.sorted(otherUser, primaryNamespace: "/bekir", username: nil).first?.address == "a")
        precondition(MailboxOrder.sorted([], primaryNamespace: "/bekir", username: nil).isEmpty)

        let history = (0..<400).map { "m\($0)" }
        var window = HistoryWindow()
        window.update(history, followingLatest: true)
        precondition(window.ids == (390..<400).reversed().map { "m\($0)" })
        precondition(window.loadOlder(history) == 10)
        precondition(window.ids.count == 20 && window.ids.first == "m399" && window.ids.last == "m380")
        let reading = window.ids
        let incoming = history + ["incoming", "sent-from-another-device"]
        window.update(incoming, followingLatest: false)
        precondition(window.ids == reading, "New mail cannot move the history window")
        let removed = incoming.filter { $0 != "m385" }
        window.update(removed, followingLatest: false)
        precondition(!window.ids.contains("m385") && window.ids.first == "m399")
        while window.hasOlder(in: removed) { precondition(window.loadOlder(removed) > 0) }
        precondition(window.ids.last == "m0" && Set(window.ids).count == 399)
        precondition(window.loadOlder(removed) == 0)
        window.update(incoming, followingLatest: true)
        precondition(window.ids.count == 10 && window.ids.first == "sent-from-another-device")
        window = HistoryWindow()
        window.update([], followingLatest: true)
        precondition(window.ids.isEmpty)
        window.update(["only"], followingLatest: true)
        precondition(window.ids == ["only"] && !window.hasOlder(in: ["only"]))
        window.update([], followingLatest: false)
        precondition(window.ids.isEmpty)
        print("iOS mailbox ordering and history window: passed")
    }
}
