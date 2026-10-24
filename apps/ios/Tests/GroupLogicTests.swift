// Groups without a network or the library: the encrypted body's shape (it must match the web
// client's encodeBody/decodeBody), notice wording, the security lines a verified commit produces,
// timers, local expiry, join links and the vault's sealing. Run by Tests/run.sh.
import CryptoKit
import Foundation

var checks = 0, failures = 0
func check(_ ok: Bool, _ what: String) {
    checks += 1
    if ok { print("  ok   " + what) } else { failures += 1; print("  FAIL " + what) }
}

let me = "/k/me", alp = "/k/alp", oz = "/k/oz"
let names: (String) -> String = { [me: "You", alp: "Alp", oz: "Özgür"][$0] ?? $0 }

// ---- the body inside the encryption ----------------------------------------------------------------
let body = GroupBody(text: "hi", files: [.init(id: "gat_1", name: "a.txt", mime: "text/plain", size: 3)], replyTo: "4").encoded()
let json = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any] ?? [:]
check(json["v"] as? Int == 1 && json["type"] as? String == "text" && json["text"] as? String == "hi", "body carries v, type and text")
check(json["reply_to"] as? String == "4" && json["edit_of"] == nil, "reply_to uses the web's key; absent fields are left out")
check(((json["files"] as? [[String: Any]])?.first?["mime"] as? String) == "text/plain", "files keep id, name, mime and size")
// What the web client writes.
let web = Data(#"{"v":1,"type":"text","text":"from the browser","files":[{"id":"gat_9","name":"p.png","mime":"image/png","size":12}],"edit_of":"7"}"#.utf8)
let decoded = GroupBody.decode(web)
check(decoded.text == "from the browser" && decoded.files?.first?.name == "p.png" && decoded.editOf == "7", "a web client's body decodes")
check(GroupBody.decode(Data("plain words".utf8)).text == "plain words", "anything that is not our JSON reads as text")
check(GroupBody.decode(Data(#"{"v":1,"files":[{"id":"x"}]}"#.utf8)).files?.first?.name == "file", "a file entry missing fields is still a file")

// ---- notices ---------------------------------------------------------------------------------------
func notice(_ kind: String, actor: String? = alp, meta: NoticeMeta = NoticeMeta()) -> String? {
    GroupText.notice(GroupItem(kind: .notice, seq: 1, notice: kind, actor: actor, metadata: meta), names: names)
}
check(notice("member_invited", meta: NoticeMeta(address: oz)) == "Alp invited Özgür", "invitation notice")
check(notice("role_changed", meta: NoticeMeta(address: oz, role: "admin")) == "Alp made Özgür an admin", "role notice")
check(notice("settings_changed", meta: NoticeMeta(changed: ["post_policy"], postPolicy: "admins")) == "Alp set posting to admins only", "posting notice")
check(notice("timer_changed", meta: NoticeMeta(ephemeralSecs: 3600)) == "Alp set messages to disappear after 1 hour", "timer notice")
check(notice("timer_changed") == "Alp turned off disappearing messages", "timer off notice")
check(notice("call_ended", meta: NoticeMeta(duration: 125)) == "Call ended · 2 min", "call ended notice")
check(notice("member_joined") == nil && notice("device_added") == nil, "joins and devices come from verified commits, not notices")

// ---- verified commits ------------------------------------------------------------------------------
let mine = GroupDeviceInfo(mailbox: me, deviceId: "ios-1", fingerprint: "00")
let alpPhone = GroupDeviceInfo(mailbox: alp, deviceId: "ios-a", fingerprint: "aa")
let alpLaptop = GroupDeviceInfo(mailbox: alp, deviceId: "web-a", fingerprint: "bb")
let first = GroupItem(kind: .commit, seq: 2, committer: mine, added: [alpPhone], removed: [], updated: [])
let second = GroupItem(kind: .commit, seq: 5, committer: alpLaptop, added: [alpLaptop], removed: [], updated: [])
let own = GroupItem(kind: .commit, seq: 6, committer: mine, added: [GroupDeviceInfo(mailbox: me, deviceId: "ios-1", fingerprint: "00")], removed: [], updated: [])
let list = [first, second, own]
let joined = GroupText.commitLines(list, at: first, me: me, myDevice: "ios-1", names: names)
check(joined.count == 1 && joined[0].text == "Alp joined" && !joined[0].security, "a newcomer reads as joined")
let device = GroupText.commitLines(list, at: second, me: me, myDevice: "ios-1", names: names)
check(device.count == 1 && device[0].text == "Alp added a new device" && device[0].security && device[0].fingerprint == "bb",
      "an existing member's new device is a security line with its fingerprint")
check(GroupText.commitLines(list, at: own, me: me, myDevice: "ios-1", names: names).isEmpty, "this device is never announced to itself")

// ---- lines, expiry, previews ----------------------------------------------------------------------
let now = 1_800_000_000
let history = [
    GroupItem(kind: .message, seq: 3, createdAt: now - 60, sender: alp, text: "one"),
    GroupItem(kind: .message, seq: 4, createdAt: now - 50, sender: alp, text: "two"),
    GroupItem(kind: .message, seq: 7, createdAt: now - 40, expiresAt: now - 1, sender: oz, text: "gone"),
    GroupItem(kind: .warning, seq: 4, order: 0.5, createdAt: now - 50, warning: "membership_mismatch"),
]
let lines = GroupText.lines(history, me: me, myDevice: "ios-1", now: now, names: names, digits: { $0 })
let messages = lines.filter { $0.kind == .message }
check(messages.count == 2 && !messages.contains { $0.item?.text == "gone" }, "expired messages are dropped by the local clock")
check(messages[0].showSender && !messages[1].showSender, "a sender's name heads a run, once")
check(lines.contains { $0.kind == .warning && $0.text.hasPrefix("Security warning: a membership change") }, "a mismatch is drawn as a warning")
check(GroupText.preview(history, me: me, now: now, names: names) == "Alp: two", "the preview also drops expired messages")
check(GroupText.preview([GroupItem(kind: .message, seq: 1, sender: me, text: "mine")], me: me, names: names) == "You: mine", "own messages preview as You")

// ---- authenticated edits and group receipts -------------------------------------------------------
let original = GroupItem(kind: .message, seq: 10, createdAt: now - 100, sender: me, text: "first")
let edited = GroupItem(kind: .message, seq: 11, createdAt: now - 50, sender: me, text: "second", editOf: "10", actionsVerified: true)
let finalEdit = GroupItem(kind: .message, seq: 12, createdAt: now - 10, sender: me, text: "third", editOf: "11", actionsVerified: true)
let chain = [original, edited, finalEdit]
check(GroupActions.visible(chain, now: now).map(\.text) == ["third"], "a verified edit chain shows only its latest text")
check(GroupActions.versions(of: finalEdit, in: chain).map(\.text) == ["first", "second", "third"], "the local version history keeps every text in order")
var unverified = edited
unverified.actionsVerified = nil
check(GroupActions.visible([original, unverified], now: now).count == 2, "an older vault's unverified action never hides another message")
var mismatchedAuthor = edited
mismatchedAuthor.sender = alp
check(GroupActions.visible([original, mismatchedAuthor], now: now).count == 2, "one member cannot edit another member's message")
var outerClaim = original
outerClaim.replacedBy = 999
check(GroupActions.visible([outerClaim], now: now).count == 1, "a server replacement claim alone never hides authenticated plaintext")
for ref in ["0", "-1", "+10", "010", "10 ", "١٠", "9223372036854775808"] {
    check(GroupActions.sequence(ref) == nil, "invalid group reference is refused: \(ref)")
}
check(GroupActions.sequence("10") == 10, "canonical positive group references resolve")
var futureParent = edited
futureParent.editOf = "12"
check(GroupActions.parent(of: futureParent, in: chain) == nil, "an edit cannot point forward or form a cycle")
check(GroupActions.parent(of: edited, in: [edited]) == nil, "a missing original is not invented")
var pendingEdit = edited
pendingEdit.pending = true
check(GroupActions.visible([original, pendingEdit], now: now).count == 2, "a pending edit does not hide the delivered original")
var disappearing = original
disappearing.expiresAt = now - 1
check(GroupActions.visible([disappearing, edited, finalEdit], now: now).isEmpty, "later versions cannot outlive a known expired original")
check(GroupActions.editable(finalEdit, in: chain, me: me, now: now), "a recent own latest version can be edited")
check(!GroupActions.editable(original, in: chain, me: me, now: now), "a superseded version is not edited again")
check(!GroupActions.editable(finalEdit, in: chain, me: alp, now: now), "another member has no edit control")
check(!GroupActions.editable(finalEdit, in: chain, me: me, now: now + 86_300), "the window is measured from the original, including its exact boundary")
check(!GroupActions.editable(edited, in: [disappearing, edited], me: me, now: now), "an expired original is not editable")
let longChain = (1...2_000).map { seq in
    GroupItem(kind: .message, seq: seq, createdAt: now - 1, sender: me, text: "v\(seq)",
              editOf: seq > 1 ? String(seq - 1) : nil, actionsVerified: true)
}
check(GroupActions.visible(longChain.reversed(), now: now).map(\.seq) == [2_000], "a long out-of-order history folds iteratively")
let receiptsDecoder = JSONDecoder()
receiptsDecoder.keyDecodingStrategy = .convertFromSnakeCase
var receipt = try! receiptsDecoder.decode(GroupReceipts.self, from: Data(#"{"members":[{"address":"/k/a","delivered_seq":20,"seen_seq":15},{"address":"/k/b","delivered_seq":20,"seen_seq":null}],"read_receipts":true}"#.utf8))
check(receipt.state(at: 10) == .seen, "all members sharing a read cursor must have seen it")
check(receipt.state(at: 16) == .delivered, "one visible reader behind the message keeps it at delivered")
check(receipt.state(at: 21) == .sent, "all members must receive it before delivered is shown")
receipt.readReceipts = false
check(receipt.state(at: 10) == .delivered, "the caller's read-receipt opt-out masks seen")
receipt.readReceipts = true
receipt.members?[0].seenSeq = nil
check(receipt.state(at: 10) == .delivered, "all members opting out cannot produce a green tick")
receipt.members = []
check(receipt.state(at: 10) == .sent, "an empty group receipt list makes no delivery claim")

// ---- timers, faces, links ----------------------------------------------------------------------------
check(GroupTimers.all.count == 7 && GroupTimers.label(nil) == "Off" && GroupTimers.label(2419200) == "4 weeks", "the seven timers and their names")
check(GroupText.initials("Garden club") == "GC" && GroupText.initials("x") == "X" && GroupText.initials("") == "··", "group initials")
check(GroupText.spaced("1234567890123") == "12345 67890 123", "safety numbers read in fives")
let link = GroupJoinLink(url: URL(string: "https://inbox.pigeonpost.dev/g/grp_abc234#tok_xyz")!)
check(link?.groupId == "grp_abc234" && link?.token == "tok_xyz", "a join link gives the group and its token")
check(GroupJoinLink(url: URL(string: "https://inbox.pigeonpost.dev/g/grp_abc234")!)?.token == nil, "a link without a token is a listed group")
check(GroupJoinLink(url: URL(string: "https://evil.example/g/grp_abc234#t")!) == nil
      && GroupJoinLink(url: URL(string: "https://inbox.pigeonpost.dev/g/../x")!) == nil
      && GroupJoinLink(url: URL(string: "http://inbox.pigeonpost.dev/g/grp_a#t")!) == nil, "anything else is not a join link")

// ---- the vault -----------------------------------------------------------------------------------------
let dir = FileManager.default.temporaryDirectory.appendingPathComponent("pp-vault-\(UUID().uuidString)")
let key = SymmetricKey(size: .bits256)
let vault = GroupVault(directory: dir, key: { key })
var record = GroupRecord(deviceId: "ios-1", stateKey: Data(repeating: 7, count: 32))
record.history["grp_1"] = [GroupItem(kind: .message, seq: 1, sender: alp, text: "a secret sentence")]
try? vault.save(record, for: me)
let files = (try? FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)) ?? []
let raw = files.first.flatMap { try? Data(contentsOf: $0) } ?? Data()
check(files.count == 1 && raw.range(of: Data("a secret sentence".utf8)) == nil, "the sealed file holds no plaintext")
check(vault.load(me) == record, "it opens again with the same key")
check(GroupVault(directory: dir, key: { SymmetricKey(size: .bits256) }).load(me) == nil, "and not with another key")
if let file = files.first { try? FileManager.default.copyItem(at: file, to: dir.appendingPathComponent("copy")) }
check(vault.load(oz) == nil, "nor as another mailbox's record")
vault.removeAll()

print("Groups logic: \(checks) checks, \(failures) failures")
exit(failures == 0 ? 0 : 1)
