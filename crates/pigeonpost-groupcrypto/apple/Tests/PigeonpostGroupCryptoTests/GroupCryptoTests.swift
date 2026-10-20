import Foundation
import XCTest
@testable import PigeonpostGroupCrypto

final class GroupCryptoTests: XCTestCase {
    let group = "grp_swiftpm"

    func device(_ seed: UInt8, _ id: String) throws -> GroupDevice {
        try GroupDevice.newWithIdentitySeed(identitySeed: Data(repeating: seed, count: 32), deviceId: id)
    }

    func testLifecycle() throws {
        let mac = try device(1, "mac"), phone = try device(2, "iphone"), pad = try device(3, "ipad")
        _ = try mac.createGroup(groupId: group)
        let kps = try phone.generateKeyPackages(count: 1) + pad.generateKeyPackages(count: 1)
        let commit = try mac.addMembers(groupId: group, keyPackages: kps)
        try verifyEnvelopeSignature(deviceSignatureKey: mac.signaturePublicKey(), message: commit.commit, signature: commit.envelopeSignature)
        _ = try mac.confirmCommit(groupId: group)
        XCTAssertEqual(try phone.joinFromWelcome(welcome: commit.welcome!).epoch, 1)
        _ = try pad.joinFromWelcome(welcome: commit.welcome!)
        XCTAssertEqual(try mac.safetyNumber(groupId: group), try pad.safetyNumber(groupId: group))

        let file = try sealAttachment(plaintext: Data("photo".utf8))
        let sent = try mac.encryptMessage(groupId: group, clientMessageId: "m-1", body: Data("hi".utf8), attachments: [file.reference])
        let header = try inspectEnvelope(message: sent.ciphertext)
        XCTAssertEqual(header.kind, "application")
        XCTAssertEqual(header.senderDevice, "mac")
        guard case let .message(got) = try phone.processIncoming(groupId: group, message: sent.ciphertext) else {
            return XCTFail("expected a message")
        }
        XCTAssertEqual(got.body, Data("hi".utf8))
        XCTAssertEqual(try openAttachment(sealed: file.sealed, reference: got.attachments[0]), Data("photo".utf8))

        // Remove the iPad; it learns of its removal and cannot read what follows.
        let removal = try phone.removeMailbox(groupId: group, mailbox: pad.mailbox())
        _ = try phone.confirmCommit(groupId: group)
        _ = try mac.processIncoming(groupId: group, message: removal.commit)
        guard case let .commit(outcome) = try pad.processIncoming(groupId: group, message: removal.commit) else {
            return XCTFail("expected a commit")
        }
        XCTAssertTrue(outcome.selfRemoved)
        let after = try mac.encryptMessage(groupId: group, clientMessageId: "m-2", body: Data("secret".utf8), attachments: [])
        XCTAssertThrowsError(try pad.processIncoming(groupId: group, message: after.ciphertext)) { error in
            guard case GroupCryptoError.Failure(let code, _) = error else { return XCTFail("\(error)") }
            XCTAssertEqual(code, "unknown_group")
        }

        // State survives an encrypted export/import.
        let key = try generateStateKey()
        let restored = try GroupDevice.importState(blob: try phone.exportState(key: key), key: key)
        guard case .message = try restored.processIncoming(groupId: group, message: after.ciphertext) else {
            return XCTFail("restored device should decrypt")
        }
    }
}
