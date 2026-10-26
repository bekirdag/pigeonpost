import Foundation

struct RoomOrganizationMailbox: Decodable, Equatable {
    let address: String
    let handle: String?
}

struct RoomOrganizationSettings: Decodable, Equatable {
    let roomsCreation: String?
}

struct RoomOrganization: Decodable, Identifiable, Equatable {
    let id: String
    let name: String
    let namespace: String?
    let status: String
    let settings: RoomOrganizationSettings?
    let myRoles: [String]
    let myMemberId: String?
    let myMailbox: RoomOrganizationMailbox?

    var readable: Bool { ["active", "past_due", "read_only"].contains(status) }
    var writable: Bool { ["active", "past_due"].contains(status) }

    /// A UI hint only. The server rechecks membership, status and room-creation policy.
    func mayCreate(as mailbox: String) -> Bool {
        guard writable, myMailbox?.address == mailbox else { return false }
        switch settings?.roomsCreation {
        case "everyone": return true
        case "admins": return !myRoles.isEmpty
        default: return false
        }
    }
}

struct RoomOperatedMailbox: Decodable, Equatable {
    let address: String
    let handle: String?
    let orgId: String
    let capabilities: [String]?
}

struct RoomOrganizationsResponse: Decodable, Equatable {
    let orgs: [RoomOrganization]
    let operated: [RoomOperatedMailbox]?

    /// Only canonical addresses from live membership/delegation metadata associate an inbox.
    func organizations(for mailbox: String) -> [RoomOrganization] {
        let operatedIds = Set((operated ?? []).filter {
            $0.address == mailbox && $0.capabilities?.contains("read_inbox") == true
        }.map(\.orgId))
        return orgs.filter { $0.myMailbox?.address == mailbox || operatedIds.contains($0.id) }
    }
}

struct OrganizationRoom: Decodable, Identifiable, Equatable {
    let id: String
    let name: String
    let description: String?
    let memberCount: Int
    let myState: String?
    let joinPolicy: String

    enum Action { case open, acceptInvitation, join, unavailable }
    var action: Action {
        switch myState {
        case "active", "keying": return .open
        case "invited": return .acceptInvitation
        case nil, "left", "removed": return joinPolicy == "public" ? .join : .unavailable
        default: return .unavailable
        }
    }
}

struct OrganizationRoomsResponse: Decodable, Equatable { let rooms: [OrganizationRoom] }

enum OrganizationRoomJoinPolicy: String, CaseIterable {
    case members = "public"
    case invited = "private"
}

enum OrganizationRoomPostPolicy: String, CaseIterable {
    case everyone, admins
}

enum OrganizationRoomInput {
    static func identifier(_ value: String) throws -> String {
        guard !value.isEmpty, value.utf8.count <= 128, value.utf8.allSatisfy({ byte in
            (48...57).contains(byte) || (65...90).contains(byte) || (97...122).contains(byte) || byte == 45 || byte == 95
        }) else { throw APIError(status: 0, code: "room_input", detail: "This room or organization is invalid.") }
        return value
    }
}

/// The exact encoded create request, retained unchanged for an explicit uncertain-outcome retry.
struct OrganizationRoomCreatePayload: Equatable {
    let organizationId: String
    let requestKey: String
    let name: String
    let description: String
    let joinPolicy: OrganizationRoomJoinPolicy
    let postPolicy: OrganizationRoomPostPolicy
    let listed: Bool
    let data: Data

    init(organizationId: String, name: String, description: String,
         joinPolicy: OrganizationRoomJoinPolicy, postPolicy: OrganizationRoomPostPolicy,
         listed: Bool, requestKey: String) throws {
        self.organizationId = try OrganizationRoomInput.identifier(organizationId)
        let name = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let description = description.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty, name.unicodeScalars.count <= 80,
              !name.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }),
              description.unicodeScalars.count <= 500, (8...128).contains(requestKey.utf8.count) else {
            throw APIError(status: 0, code: "room_input", detail: "Use a name of 1–80 characters and a description of at most 500 characters.")
        }
        _ = try OrganizationRoomInput.identifier(requestKey)
        self.requestKey = requestKey
        self.name = name; self.description = description
        self.joinPolicy = joinPolicy; self.postPolicy = postPolicy; self.listed = listed
        var fields: [String: Any] = [
            "request_key": requestKey, "org_id": organizationId, "name": name,
            "join_policy": joinPolicy.rawValue, "post_policy": postPolicy.rawValue,
            "visibility": listed ? "listed" : "unlisted", "history_visible_to_new_members": false,
        ]
        if !description.isEmpty { fields["description"] = description }
        data = try JSONSerialization.data(withJSONObject: fields, options: [.sortedKeys, .withoutEscapingSlashes])
    }
}
