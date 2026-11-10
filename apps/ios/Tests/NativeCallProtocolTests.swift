// Run by Tests/run.sh: the plain-text parts of the native call engine, checked against what the web
// page produces. The expected values come from running the same input through calls.js's logic.
import Foundation

var failures = 0
func check(_ condition: Bool, _ name: String) {
    print((condition ? "PASS " : "FAIL ") + name)
    if !condition { failures += 1 }
}

// The event stream
var parser = NativeCallProtocol.EventParser()
func feed(_ text: String) -> [NativeCallProtocol.ServerEvent] { text.utf8.compactMap { parser.feed($0) } }
check(feed("event: ready\ndata: {\"session\":\"s1\"}\n\n") == [.init(name: "ready", data: "{\"session\":\"s1\"}")], "a frame is read whole")
check(feed(": keep-alive\n\n").isEmpty, "a keep-alive comment is no event")
check(feed("event: signal\nda").isEmpty && feed("ta: {}\n\n") == [.init(name: "signal", data: "{}")], "a frame split across reads is joined")
check(feed("event: a\ndata: 1\n\nevent: b\ndata: 2\n\n").map(\.name) == ["a", "b"], "two frames in one read are both delivered")
check(feed("data: only\n\n").isEmpty, "a frame with no name is dropped")

// Who is in the room
let room = NativeCallProtocol.RoomView(["room": "r", "you": "me", "participants": [["participant": "me", "name": "A"], ["participant": "them"]]])
check(room?.others == [.init(id: "them", name: "Someone")], "this device offers to everyone else, and only them")
check(room?.topology == "mesh", "a room with no topology is a mesh")
check(NativeCallProtocol.RoomView(["you": "me"]) == nil, "a view with no room is refused")

// Opus
let tuned = NativeCallProtocol.tuneOpus("m=audio 9 UDP 111\r\na=rtpmap:111 opus/48000/2\r\na=fmtp:111 minptime=20;useinbandfec=1\r\na=mid:0\r\n")
check(tuned.contains("a=fmtp:111 minptime=10;useinbandfec=1;usedtx=0;stereo=0;sprop-stereo=0;maxplaybackrate=48000;maxaveragebitrate=128000\r\n"), "existing Opus parameters are replaced, in the page's order")
let added = NativeCallProtocol.tuneOpus("a=rtpmap:109 OPUS/48000/2\r\na=mid:0\r\n")
check(added.contains("a=rtpmap:109 OPUS/48000/2\r\na=fmtp:109 minptime=10;"), "missing Opus parameters are added after the codec line")
check(NativeCallProtocol.tuneOpus("m=video 9 UDP 96\r\n") == "m=video 9 UDP 96\r\n", "a description with no Opus is untouched")

// The security code. Expected value from Node: SHA-256 of
// "pigeonpost-call-sas-v2\nAA:BB\nCC:DD", first 8 bytes big-endian, modulo 10^12.
let code = NativeCallProtocol.securityCode(localSDP: "a=fingerprint:sha-256 cc:dd\r\n", remoteSDP: "a=fingerprint:sha-256 AA:BB\r\n")
check(code == "3642 1893 3769", "the security code matches the web page's")
check(code == NativeCallProtocol.securityCode(localSDP: "a=fingerprint:sha-256 AA:BB", remoteSDP: "a=fingerprint:sha-256 CC:DD"), "both ends compute one code")
check(NativeCallProtocol.securityCode(localSDP: nil, remoteSDP: "x") == nil, "no description, no code")

// Candidates
check(NativeCallProtocol.candidateType("candidate:1 1 udp 1 203.0.113.9 5000 typ srflx raddr 0.0.0.0") == "srflx", "the candidate type is read")
check(NativeCallProtocol.candidateType("nothing") == nil, "no type, no answer")

exit(failures == 0 ? 0 : 1)
