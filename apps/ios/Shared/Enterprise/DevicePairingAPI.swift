import Foundation

extension PostboxClient {
    func resolveDevicePairing(_ code: DevicePairingCode) async throws -> DevicePairingPreview {
        let data = try await sendRaw("/v1/device-pairings/resolve", method: "POST", json: ["user_code": code.displayValue], timeout: 20)
        guard let preview = try? Self.decode(DevicePairingPreview.self, from: data), preview.isValid else {
            throw Self.unverifiedPairingResponse
        }
        return preview
    }

    func approveDevicePairing(_ preview: DevicePairingPreview, code: DevicePairingCode) async throws {
        guard preview.allowsApproval(at: Date()) else {
            throw APIError(status: 409, code: "pairing_not_actionable", detail: "Resolve this device request again before approving it.")
        }
        let data = try await sendRaw("/v1/device-pairings/" + preview.pairingId + "/approve", method: "POST", json: ["user_code": code.displayValue], timeout: 20)
        struct Acknowledgment: Decodable { let approved: Bool; let address: String; let handle: String }
        guard let result = try? Self.decode(Acknowledgment.self, from: data), result.approved,
              result.handle == preview.handle, result.address.hasPrefix("/k/"), result.address.utf8.count <= 160,
              preview.address == nil || preview.address == result.address else { throw Self.unverifiedPairingResponse }
    }

    /// Resolve again after this acknowledgment: a concurrent approval can already have won.
    func denyDevicePairing(_ preview: DevicePairingPreview, code: DevicePairingCode) async throws {
        guard preview.isValid, preview.state == nil || preview.state == "pending" else {
            throw APIError(status: 409, code: "pairing_not_actionable", detail: "Resolve this device request again before denying it.")
        }
        let data = try await sendRaw("/v1/device-pairings/" + preview.pairingId + "/deny", method: "POST", json: ["user_code": code.displayValue], timeout: 20)
        struct Acknowledgment: Decodable { let denied: Bool }
        guard let result = try? Self.decode(Acknowledgment.self, from: data), result.denied else { throw Self.unverifiedPairingResponse }
    }

    private static var unverifiedPairingResponse: APIError {
        APIError(status: 200, code: "bad_response", detail: "The device request could not be verified. Resolve it again to check its status.")
    }
}
