//  Incoming-call rings: the payload shapes of docs/call-alerting.md, the room → CallKit UUID rule,
//  the end-reason mapping, expiry, and the ring-state responses. Pure functions over their inputs,
//  compiled with plain `swiftc` by Tests/run.sh.

import Foundation

nonisolated(unsafe) var failures = 0
func check(_ condition: Bool, _ what: String) {
    if condition { print("  ok   \(what)") } else { failures += 1; print("  FAIL \(what)") }
}
func equal<T: Equatable>(_ actual: T, _ expected: T, _ what: String) {
    if actual == expected { print("  ok   \(what)") } else { failures += 1; print("  FAIL \(what) — got \(actual), wanted \(expected)") }
}

@main
enum CallRingTests {
    static func main() {
        let token = String(repeating: "A", count: 21) + "-_" + String(repeating: "z", count: 20)   // 43 characters
        let room = "r0Om1d2a3b4c5d6e7f8g9h"   // 22 characters, as the calls service mints them

        func json(_ text: String) -> [AnyHashable: Any] {
            (try! JSONSerialization.jsonObject(with: Data(text.utf8))) as! [AnyHashable: Any]
        }

        print("VoIP push payload")
        let voip = json("""
        {"aps":{},"type":"call_ring","room":"\(room)","code":"ABC234","kind":"video","ring_token":"\(token)",
         "from":{"address":"/k/alp000000000000000000000a","handle":"/alp","display":"Alp"},
         "to":"/bekir","identity":"/k/bekir0000000000000000000a","expires_at":1792500045}
        """)
        let ring = CallRing(push: voip)
        check(ring != nil, "a ring push parses")
        equal(ring?.room, room, "room")
        equal(ring?.kind, "video", "kind")
        equal(ring?.isVideo, true, "video")
        equal(ring?.code, "ABC234", "code")
        equal(ring?.ringToken, token, "ring token")
        equal(ring?.fromHandle, "/alp", "caller handle")
        equal(ring?.fromAddress, "/k/alp000000000000000000000a", "caller address")
        equal(ring?.handleValue, "/alp", "CallKit handle prefers the handle")
        equal(ring?.callerName { _ in "fallback" }, "Alp", "caller name is the display name")
        equal(ring?.callerName(known: "Alp from work") { _ in "fallback" }, "Alp from work", "a name from the inbox wins")
        equal(ring?.identity, "/k/bekir0000000000000000000a", "identity")
        equal(ring?.expiresAt, Date(timeIntervalSince1970: 1792500045), "expires_at in seconds")

        print("Mac alert payload (same fields plus aps.alert)")
        let alert = json("""
        {"aps":{"alert":{"title":"Alp","body":"Incoming video call"},"sound":"default","category":"CALL_RING",
         "interruption-level":"time-sensitive","thread-id":"calls"},
         "type":"call_ring","room":"\(room)","kind":"audio","ring_token":"\(token)",
         "from":{"address":"/k/alp000000000000000000000a","handle":null},"to":"/bekir"}
        """)
        let macRing = CallRing(push: alert)
        equal(macRing?.isVideo, false, "audio ring")
        equal(macRing?.fromHandle, nil, "a null handle is no handle")
        equal(macRing?.handleValue, "/k/alp000000000000000000000a", "no handle: the address is the handle")
        equal(macRing?.callerName { "name:\($0 ?? "-")" }, "name:/k/alp000000000000000000000a", "no display name: fallback of the address")
        equal(macRing?.expiresAt, nil, "no expiry when none is sent")
        equal(macRing?.isExpired(), false, "no expiry never expires here")

        print("FCM-style flat strings")
        let flat = CallRing(push: ["type": "call_ring", "room": room, "kind": "video", "ring_token": token,
                                   "from_handle": "/alp", "from_display": "Alp", "expires_at": "1792500045"])
        equal(flat?.fromHandle, "/alp", "flat handle")
        equal(flat?.expiresAt, Date(timeIntervalSince1970: 1792500045), "expiry as a string")

        print("Malformed pushes")
        check(CallRing(push: [:]) == nil, "empty payload is not a ring")
        check(CallRing(push: ["type": "message", "room": room]) == nil, "a message push is not a ring")
        check(CallRing(push: ["type": "call_ring"]) == nil, "a ring with no room is not usable")
        check(CallRing(push: ["type": "call_ring", "room": "../../v1/x"]) == nil, "a room id that is not base64url is refused")
        check(CallRing(push: ["type": "call_ring", "room": String(repeating: "a", count: 200)]) == nil, "an absurdly long room id is refused")
        equal(CallRing(push: ["type": "call_ring", "room": room, "ring_token": "short"])?.ringToken, nil, "a malformed ring token is dropped, the ring kept")
        equal(CallRing(push: ["type": "call_ring", "room": room, "kind": "hologram"])?.kind, "audio", "an unknown kind is audio")
        let hostile = CallRing(push: ["type": "call_ring", "room": room,
                                      "from": ["display": "\u{202E}evil\u{0007} name" + String(repeating: "x", count: 100)]])
        equal(hostile?.fromDisplay?.contains("\u{202E}"), false, "bidi override stripped from the display name")
        equal(hostile?.fromDisplay?.contains("\u{0007}"), false, "control characters stripped")
        equal(hostile?.fromDisplay?.count, 60, "display name cut to 60")

        print("Page ring message")
        let page = CallRing(page: ["type": "ring", "room": room, "code": "ABC234", "kind": "audio",
                                   "from": ["address": "/k/alp000000000000000000000a", "handle": "/alp"], "to": "/bekir"])
        check(page != nil, "the page's ring parses")
        equal(page?.ringToken, nil, "a page ring has no token")
        equal(page?.uuid, ring?.uuid, "push and page ring for one room are one CallKit call")
        check(CallRing(page: ["type": "ring_ended", "room": room]) == nil, "ring_ended is not a ring")

        print("Room → UUID")
        equal(CallRing.uuid(forRoom: room), CallRing.uuid(forRoom: room), "deterministic")
        check(CallRing.uuid(forRoom: room) != CallRing.uuid(forRoom: room + "x"), "different rooms, different calls")
        let uuidText = CallRing.uuid(forRoom: room).uuidString
        equal(Array(uuidText)[14], "5", "version 5 shape")
        check("89AB".contains(Array(uuidText)[19]), "RFC 4122 variant")

        print("Expiry")
        let soon = CallRing(room: room, kind: "audio", expiresAt: Date(timeIntervalSince1970: 1000))
        equal(soon.isExpired(at: Date(timeIntervalSince1970: 999)), false, "before the end time")
        equal(soon.isExpired(at: Date(timeIntervalSince1970: 1000)), true, "at the end time")
        equal(soon.isExpired(at: Date(timeIntervalSince1970: 1001)), true, "after it")

        print("Whose ring")
        let mine: [(address: String, handle: String?)] = [("/k/bekir0000000000000000000a", "/bekir"), ("/k/agent00000000000000000000a", nil)]
        equal(ring?.isFor(mailboxes: mine), true, "identity matches an address on the account")
        equal(CallRing(room: room, kind: "audio", to: "/bekir").isFor(mailboxes: mine), true, "a handle matches")
        equal(CallRing(room: room, kind: "audio", to: "/other", identity: "/k/other0000000000000000000a").isFor(mailboxes: mine), false, "another account's ring")
        equal(CallRing(room: room, kind: "audio").isFor(mailboxes: mine), true, "a ring naming nobody is given the benefit of the doubt")

        print("End reasons")
        equal(RingEnd(reason: "answered_elsewhere"), .answeredElsewhere, "answered_elsewhere")
        equal(RingEnd(reason: "declined"), .declinedElsewhere, "declined")
        equal(RingEnd(reason: "busy"), .declinedElsewhere, "busy")
        equal(RingEnd(reason: "no_answer"), .unanswered, "no_answer")
        equal(RingEnd(reason: "cancelled"), .remoteEnded, "cancelled")
        equal(RingEnd(reason: "ended"), .remoteEnded, "ended")
        equal(RingEnd(reason: nil), .remoteEnded, "no reason")
        equal(RingEnd(reason: "something new"), .remoteEnded, "an unknown reason")

        print("Ring state responses")
        equal(RingState(status: 200, body: Data(#"{"state":"ringing","kind":"video","expires_at":1792500045}"#.utf8)),
              .ringing(expiresAt: Date(timeIntervalSince1970: 1792500045)), "ringing")
        equal(RingState(status: 200, body: Data(#"{"state":"ended","reason":"answered_elsewhere"}"#.utf8)),
              .ended(.answeredElsewhere), "ended elsewhere")
        equal(RingState(status: 200, body: Data(#"{"state":"ended","reason":"cancelled"}"#.utf8)), .ended(.remoteEnded), "caller hung up")
        equal(RingState(status: 404, body: Data(#"{"error":"no_ring"}"#.utf8)), .ended(.remoteEnded), "404 no_ring is over")
        equal(RingState(status: 502, body: Data()), nil, "a gateway error is unknown, not over")
        equal(RingState(status: 200, body: Data("not json".utf8)), nil, "garbage is unknown")

        print("Endpoints follow the page's origin")
        let production = URL(string: "https://inbox.pigeonpost.dev/native-call.html?v=20261019-native")!
        let harness = URL(string: "http://127.0.0.1:8460/native-call.html")!
        equal(RingEndpoint.state(page: production, room: room)?.absoluteString, "https://inbox.pigeonpost.dev/calls/v1/rings/\(room)", "state, production")
        equal(RingEndpoint.decline(page: production, room: room)?.absoluteString, "https://inbox.pigeonpost.dev/calls/v1/rings/\(room)/decline", "decline, production")
        equal(RingEndpoint.decline(page: harness, room: room)?.absoluteString, "http://127.0.0.1:8460/calls/v1/rings/\(room)/decline", "decline, harness")

        print(failures == 0 ? "call rings: all passed" : "call rings: \(failures) failed")
        exit(failures == 0 ? 0 : 1)
    }
}
