// Swift interop agent: the generated UniFFI Swift binding (bindings/swift) linked against the
// Rust library, as the iOS/macOS apps will use it. Protocol: interop/run_interop.py.
import Foundation

typealias J = [String: Any]

var device: GroupDevice?

func b64(_ d: Data) -> String { d.base64EncodedString() }
func data(_ j: J, _ k: String) -> Data { Data(base64Encoded: j[k] as? String ?? "") ?? Data() }
func str(_ j: J, _ k: String) -> String { j[k] as? String ?? "" }

func commitJson(_ b: CommitBundle) -> J {
    ["epoch": b.epoch, "commit_id": b.commitId, "commit": b64(b.commit),
     "welcome": b.welcome.map(b64) ?? NSNull(), "signature": b64(b.envelopeSignature)]
}

func attJson(_ a: AttachmentRef) -> J { ["key": b64(a.key), "sha": b64(a.sealedSha256), "len": a.plaintextLen] }

func attIn(_ j: J) -> AttachmentRef {
    AttachmentRef(key: data(j, "key"), sealedSha256: data(j, "sha"), plaintextLen: (j["len"] as? NSNumber)?.uint64Value ?? 0)
}

func incomingJson(_ i: Incoming) -> J {
    switch i {
    case .message(let m):
        return ["type": "message", "body": b64(m.body), "sender_mailbox": m.sender.mailbox,
                "sender_device": m.sender.deviceId, "client_message_id": m.clientMessageId,
                "epoch": m.epoch, "attachments": m.attachments.map(attJson)]
    case .commit(let o):
        return ["type": "commit", "epoch": o.epoch, "self_removed": o.selfRemoved,
                "committer_mailbox": o.committer.mailbox, "added": o.added.count, "removed": o.removed.count]
    case .ownCommitConfirmed(let o):
        return ["type": "own_commit_confirmed", "epoch": o.epoch]
    case .historyOffer:
        return ["type": "history_offer"]
    case .own:
        return ["type": "own"]
    }
}

func handle(_ r: J) throws -> J {
    let g = str(r, "group")
    switch str(r, "op") {
    case "init":
        let d = try GroupDevice.newWithIdentitySeed(identitySeed: data(r, "seed"), deviceId: str(r, "device"))
        device = d
        return ["mailbox": d.mailbox(), "signature_key": b64(d.signaturePublicKey())]
    case "key_packages":
        var kps = try device!.generateKeyPackages(count: (r["count"] as? NSNumber)?.uint32Value ?? 1)
        kps.append(try device!.generateLastResortKeyPackage())
        return ["key_packages": kps.map(b64)]
    case "create":
        return ["epoch": try device!.createGroup(groupId: g).epoch]
    case "add":
        let kps = (r["key_packages"] as? [String] ?? []).map { Data(base64Encoded: $0) ?? Data() }
        return commitJson(try device!.addMembers(groupId: g, keyPackages: kps))
    case "remove_mailbox":
        return commitJson(try device!.removeMailbox(groupId: g, mailbox: str(r, "mailbox")))
    case "self_update":
        return commitJson(try device!.selfUpdate(groupId: g))
    case "confirm":
        return ["epoch": try device!.confirmCommit(groupId: g).epoch]
    case "discard":
        try device!.discardCommit(groupId: g)
        return [:]
    case "join":
        let j = try device!.joinFromWelcome(welcome: data(r, "welcome"))
        return ["group": j.groupId, "epoch": j.epoch, "members": j.members.count]
    case "encrypt":
        var atts: [AttachmentRef] = []
        var sealed: Any = NSNull()
        if r["attachment"] is String {
            let s = try sealAttachment(plaintext: data(r, "attachment"))
            sealed = b64(s.sealed)
            atts.append(s.reference)
        }
        let m = try device!.encryptMessage(groupId: g, clientMessageId: str(r, "id"), body: data(r, "body"), attachments: atts)
        return ["ciphertext": b64(m.ciphertext), "signature": b64(m.envelopeSignature), "epoch": m.epoch, "sealed": sealed]
    case "process":
        return incomingJson(try device!.processIncoming(groupId: g, message: data(r, "message")))
    case "open_attachment":
        return ["plaintext": b64(try openAttachment(sealed: data(r, "sealed"), reference: attIn(r["ref"] as? J ?? [:])))]
    case "status":
        return ["epoch": try device!.epoch(groupId: g),
                "epoch_authenticator": b64(try device!.epochAuthenticator(groupId: g)),
                "safety_number": try device!.safetyNumber(groupId: g),
                "members": try device!.members(groupId: g).count]
    case "persist":
        let key = try generateStateKey()
        let blob = try device!.exportState(key: key)
        device = try GroupDevice.importState(blob: blob, key: key)
        return ["bytes": blob.count]
    case "snapshot":
        let key = try generateStateKey()
        return ["state": b64(try device!.exportState(key: key)), "key": b64(key)]
    case "probe_snapshot":
        let d = try GroupDevice.importState(blob: data(r, "state"), key: data(r, "key"))
        do {
            return ["code": "ok", "result": incomingJson(try d.processIncoming(groupId: g, message: data(r, "message")))]
        } catch GroupCryptoError.Failure(let code, _) {
            return ["code": code]
        }
    default:
        throw GroupCryptoError.Failure(code: "invalid_input", detail: "unknown op")
    }
}

while let line = readLine() {
    guard let req = try? JSONSerialization.jsonObject(with: Data(line.utf8)) as? J else { break }
    if str(req, "op") == "quit" { break }
    var resp: J
    do {
        resp = try handle(req)
        resp["ok"] = true
    } catch GroupCryptoError.Failure(let code, let message) {
        resp = ["ok": false, "code": code, "message": message]
    }
    let out = try JSONSerialization.data(withJSONObject: resp)
    FileHandle.standardOutput.write(out)
    FileHandle.standardOutput.write(Data("\n".utf8))
}
