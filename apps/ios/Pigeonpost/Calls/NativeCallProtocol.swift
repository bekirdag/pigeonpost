//  The parts of the call protocol that are plain text and arithmetic, kept apart from WebRTC so they
//  can be tested with `Tests/run.sh`. Each one has to agree with `site-inbox/calls.js`: the two ends
//  of a call may be this app and a browser.

import CryptoKit
import Foundation

enum NativeCallProtocol {
    /// One `event:` / `data:` frame of the calls service's event stream.
    struct ServerEvent: Equatable {
        let name: String
        let data: String
    }

    /// Splits the event stream into frames, a byte at a time. Keep-alive comments and frames with no
    /// name or data are dropped, as the page drops them.
    struct EventParser {
        private var buffer: [UInt8] = []
        /// A frame larger than this is not one the service sends.
        private let limit = 256 * 1024

        mutating func feed(_ byte: UInt8) -> ServerEvent? {
            buffer.append(byte)
            if buffer.count > limit { buffer.removeAll(keepingCapacity: true); return nil }
            guard byte == 0x0A, buffer.count >= 2, buffer[buffer.count - 2] == 0x0A else { return nil }
            let frame = String(decoding: buffer, as: UTF8.self)
            buffer.removeAll(keepingCapacity: true)
            var name: String?, data: String?
            for line in frame.split(separator: "\n") {
                if line.hasPrefix("event: ") { name = String(line.dropFirst(7)) }
                else if line.hasPrefix("data: ") { data = String(line.dropFirst(6)) }
            }
            guard let name, let data else { return nil }
            return ServerEvent(name: name, data: data)
        }
    }

    /// Who is in a room, as `joined` and the accept response describe it.
    struct RoomView: Equatable {
        struct Member: Equatable {
            let id: String
            let name: String
        }
        let room: String
        let you: String?
        let topology: String
        let members: [Member]

        init?(_ value: [String: Any]) {
            guard let room = value["room"] as? String else { return nil }
            self.room = room
            you = value["you"] as? String
            topology = (value["topology"] as? String) ?? "mesh"
            members = ((value["participants"] as? [[String: Any]]) ?? []).compactMap { entry in
                guard let id = entry["participant"] as? String else { return nil }
                return Member(id: id, name: (entry["name"] as? String) ?? "Someone")
            }
        }

        /// Everyone else, each of whom this device offers to: the newcomer always starts.
        var others: [Member] { members.filter { $0.id != you } }
    }

    /// Opus as this end wants to receive it. Same parameters as `tuneOpus` in calls.js.
    static func tuneOpus(_ sdp: String, bitrate: Int = 128_000) -> String {
        guard let payload = firstCapture(#"a=rtpmap:(\d+) opus/48000/2"#, in: sdp, options: .caseInsensitive) else { return sdp }
        let wanted: [(String, String)] = [
            ("minptime", "10"), ("useinbandfec", "1"), ("usedtx", "0"), ("stereo", "0"),
            ("sprop-stereo", "0"), ("maxplaybackrate", "48000"), ("maxaveragebitrate", String(bitrate)),
        ]
        guard let line = try? NSRegularExpression(pattern: "a=fmtp:\(payload) ([^\\r\\n]*)") else { return sdp }
        let whole = NSRange(sdp.startIndex..., in: sdp)
        if let found = line.firstMatch(in: sdp, range: whole), let body = Range(found.range(at: 1), in: sdp), let all = Range(found.range, in: sdp) {
            var params: [(String, String)] = []
            for pair in sdp[body].split(separator: ";", omittingEmptySubsequences: false) {
                let parts = pair.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
                let key = String(parts[0]).trimmingCharacters(in: .whitespaces)
                if !key.isEmpty { params.append((key, parts.count > 1 ? String(parts[1]).trimmingCharacters(in: .whitespaces) : "")) }
            }
            for (key, value) in wanted {
                if let index = params.firstIndex(where: { $0.0 == key }) { params[index].1 = value } else { params.append((key, value)) }
            }
            return sdp.replacingCharacters(in: all, with: "a=fmtp:\(payload) " + params.map { "\($0.0)=\($0.1)" }.joined(separator: ";"))
        }
        guard let rtpmap = try? NSRegularExpression(pattern: "a=rtpmap:\(payload) [^\\r\\n]*\\r?\\n"),
              let found = rtpmap.firstMatch(in: sdp, range: whole), let all = Range(found.range, in: sdp) else { return sdp }
        let text = wanted.map { "\($0.0)=\($0.1)" }.joined(separator: ";")
        return sdp.replacingCharacters(in: all, with: sdp[all] + "a=fmtp:\(payload) \(text)\r\n")
    }

    /// The twelve-digit code two people can read to each other, from the two DTLS fingerprints. The
    /// same input and format as `verifySecurity` in calls.js, so both ends show one code.
    static func securityCode(localSDP: String?, remoteSDP: String?) -> String? {
        guard let mine = fingerprint(localSDP), let theirs = fingerprint(remoteSDP) else { return nil }
        let sorted = [mine, theirs].sorted()
        let digest = Array(SHA256.hash(data: Data("pigeonpost-call-sas-v2\n\(sorted[0])\n\(sorted[1])".utf8)))
        let value = digest[0..<8].reduce(UInt64(0)) { ($0 << 8) | UInt64($1) } % 1_000_000_000_000
        let digits = String(format: "%012llu", value)
        let a = digits.prefix(4), b = digits.dropFirst(4).prefix(4), c = digits.suffix(4)
        return "\(a) \(b) \(c)"
    }

    /// The candidate's type, for the service log's per-pair counts. Never its address.
    static func candidateType(_ candidate: String) -> String? {
        firstCapture(#"\btyp (host|srflx|prflx|relay)\b"#, in: candidate)
    }

    private static func fingerprint(_ sdp: String?) -> String? {
        guard let sdp else { return nil }
        return firstCapture(#"a=fingerprint:\S+ ([0-9A-F:]+)"#, in: sdp, options: .caseInsensitive)?.uppercased()
    }

    private static func firstCapture(_ pattern: String, in text: String, options: NSRegularExpression.Options = []) -> String? {
        guard let regex = try? NSRegularExpression(pattern: pattern, options: options),
              let match = regex.firstMatch(in: text, range: NSRange(text.startIndex..., in: text)),
              let range = Range(match.range(at: 1), in: text) else { return nil }
        return String(text[range])
    }
}
