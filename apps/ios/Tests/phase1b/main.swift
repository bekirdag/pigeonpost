// Phase 1b rules, without a network or a screen: blocking's wildcard and address checks, the
// crop geometry, call-history wording and missed-call lines, and how filtered mail is grouped.
import CoreGraphics
import Foundation

var checks = 0, failures = 0
func check(_ ok: Bool, _ what: String, line: Int = #line) {
    checks += 1
    if ok { print("  ok   \(what)") } else { failures += 1; print("  FAIL \(what) (line \(line))") }
}

// Blocking
check(Blocking.wildcard(for: "/spamco/agent") == "/spamco/*", "wildcard covers the namespace")
check(Blocking.wildcard(for: "/olga") == "/olga/*", "a bare handle's own namespace")
check(Blocking.wildcard(for: "/k/abc") == nil, "no wildcard for /k/ addresses")
check(Blocking.wildcard(for: "/github/someone") == nil, "no wildcard for provider namespaces")
check(Blocking.wildcard(for: "/pp/x") == nil, "no wildcard for /pp")
check(Blocking.wildcard(for: "/spamco/*") == nil, "no wildcard of a wildcard")
check(Blocking.validAddress("/k/abc123"), "a /k/ address is valid")
check(Blocking.validAddress("/spamco/*"), "a namespace wildcard is valid")
check(Blocking.validAddress("/spamco/agent"), "a handle is valid")
check(!Blocking.validAddress("spamco"), "a bare word is not")
check(!Blocking.validAddress("/a b"), "whitespace is not")
check(!Blocking.validAddress("/*"), "everyone is not")
check(BlockTarget(peer: "/x", name: "x", origin: .filtered).reportByDefault, "report on by default from Filtered")
check(!BlockTarget(peer: "/x", name: "x", origin: .thread).reportByDefault, "report off by default elsewhere")

// Crop
var crop = AvatarCrop(image: CGSize(width: 2000, height: 1000), view: 280)
check(crop.rect == CGRect(x: 500, y: 0, width: 1000, height: 1000), "zoom 1 is the centred short side")
crop.offset = crop.clamped(CGSize(width: 10_000, height: 50))
check(crop.offset.height == 0, "no vertical slack at zoom 1 on a landscape picture")
check(crop.rect.minX == 0, "dragged fully right shows the left edge")
crop = AvatarCrop(image: CGSize(width: 1000, height: 1000), view: 250).zoomed(to: 2)
check(crop.rect == CGRect(x: 250, y: 250, width: 500, height: 500), "zoom 2 keeps the centre")
check(AvatarCrop(image: CGSize(width: 10, height: 10), view: 100).zoomed(to: 9).zoom == AvatarCrop.maxZoom, "zoom is capped")
let shifted = AvatarCrop(image: CGSize(width: 1000, height: 1000), view: 250).zoomed(to: 2)
var moved = shifted; moved.offset = moved.clamped(CGSize(width: -1000, height: -1000))
check(moved.rect == CGRect(x: 500, y: 500, width: 500, height: 500), "clamped at the bottom-right corner")

// Call history
let now = 1_800_000_000
func entry(_ direction: String, _ outcome: String?, kind: String = "audio", peer: String? = "/k/p", handle: String? = "/bob",
           group: String? = nil, duration: Int? = nil, people: [String] = []) -> CallLogEntry {
    CallLogEntry(id: UUID().uuidString, direction: direction, outcome: outcome, kind: kind, peer: peer, peerHandle: handle,
                 participants: people, groupId: group, startedAt: now, answeredAt: nil, endedAt: nil, duration: duration, seen: false)
}
check(CallLogEntry.formatDuration(65) == "1:05", "short duration")
check(CallLogEntry.formatDuration(3725) == "1:02:05", "long duration")
check(entry("incoming", "missed", kind: "video").summary == "↙ Missed · Video", "missed video wording")
check(entry("outgoing", "answered", duration: 134).summary == "↗ Outgoing · Voice · 2:14", "answered with duration")
check(entry("outgoing", "missed").outcomeLabel == "No answer", "outgoing missed is no answer")
check(entry("incoming", "cancelled").isMissed, "a cancelled incoming call counts as missed")
check(entry("incoming", "missed", group: "grp_x", people: ["a", "b", "c"]).summary.hasSuffix("3 people"), "group size shown")
let missed = CallLogEntry.missed(in: [
    entry("incoming", "missed"),
    entry("incoming", "missed", handle: nil),
    entry("outgoing", "missed"),
    entry("incoming", "answered"),
    entry("incoming", "missed", group: "grp_1"),
    entry("incoming", "missed", peer: "/k/other", handle: "/eve"),
], keys: ["/bob", "/k/p"])
check(missed.count == 2, "only incoming 1:1 missed calls from this peer become lines")
let line = CallLogEntry.line(missed[0])
check(line.kind == .system && line.body.hasPrefix("Missed voice call · "), "the line's words")
let merged = CallLogEntry.merge([ThreadMessage(id: "a", kind: .incoming, at: now - 10, body: "x", threadId: nil),
                                 ThreadMessage(id: "b", kind: .incoming, at: now + 10, body: "y", threadId: nil)], [line])
check(merged.map(\.id) == ["a", line.id, "b"], "lines sort in by time")

// Filtered
let decoder = JSONDecoder()
decoder.keyDecodingStrategy = .convertFromSnakeCase
let mail = try! decoder.decode(InboxResponse.self, from: Data("""
{"messages":[
 {"message_id":"1","from":"/k/s","peer":"/k/s","direction":"in","body":"a","received_at":100},
 {"message_id":"2","from":"/k/o","peer":"/olga","peer_handle":"/olga","direction":"in","body":"b","received_at":300},
 {"message_id":"3","from":"/k/s","peer":"/k/s","direction":"in","body":"c","received_at":200},
 {"message_id":"4","from":"/k/x","peer":"/spamco/x","peer_handle":"/spamco/x","direction":"in","body":"d","received_at":400},
 {"message_id":"5","from":"/k/me","peer":"/olga","direction":"out","body":"e","sent_at":500}
]}
""".utf8)).messages!
let blocks = [Contact(peer: "/spamco/*", alias: nil, admission: "block", autonomy: "review", allowedVerbs: [])]
let senders = FilteredSender.group(mail, contacts: blocks)
check(senders.map(\.peer) == ["/olga", "/k/s"], "newest sender first, blocked and own mail left out")
check(senders.last?.messages.map(\.messageId) == ["1", "3"], "a sender's mail in time order")
check(senders.first?.from == "/k/o", "accept names the address it came from")

var receiptPolicy = try decoder.decode(InboxFilters.self, from: Data("{\"read_receipts\":false,\"unknown_calls\":\"ring\"}".utf8))
check(receiptPolicy["read_receipts"] == "false", "read-receipt opt-out decodes without becoming the default")
receiptPolicy.set("read_receipts", "true")
check(receiptPolicy.readReceipts == true && receiptPolicy.unknownCalls == "ring", "receipt changes preserve other policy fields")
let legacyReceiptPolicy = try decoder.decode(InboxFilters.self, from: Data("{}".utf8))
check(legacyReceiptPolicy["read_receipts"] == "true", "legacy policy retains the server's enabled default")
let receiptPatch = ["read_receipts": InboxFilters.wireValue(field: "read_receipts", value: "false")!]
let receiptWire = try JSONSerialization.jsonObject(with: JSONSerialization.data(withJSONObject: receiptPatch)) as! [String: Any]
check(receiptWire["read_receipts"] as? Bool == false && receiptWire.count == 1, "opt-out is sent as a JSON boolean and no other policy field")
check(InboxFilters.wireValue(field: "read_receipts", value: "invalid") == nil, "invalid receipt selections cannot produce a save")

print("Phase 1b: \(checks) checks, \(failures) failures")
exit(failures == 0 ? 0 : 1)
