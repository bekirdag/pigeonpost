import Foundation

extension PostboxClient {
    func roomOrganizations(_ caller: GroupCaller) async throws -> RoomOrganizationsResponse {
        let response = try await send("/v1/orgs", headers: caller.headers(), as: RoomOrganizationsResponse.self)
        guard Set(response.orgs.map(\.id)).count == response.orgs.count,
              response.orgs.allSatisfy({ (try? OrganizationRoomInput.identifier($0.id)) != nil }) else {
            throw APIError(status: 200, code: "bad_response", detail: "The organization list could not be confirmed.")
        }
        return response
    }

    /// `org_id` is mandatory: omitting it lists personal public groups instead.
    func organizationRooms(_ caller: GroupCaller, organizationId: String, search: String) async throws -> [OrganizationRoom] {
        let organizationId = try OrganizationRoomInput.identifier(organizationId)
        let query = [URLQueryItem(name: "org_id", value: organizationId),
                     URLQueryItem(name: "q", value: String(String.UnicodeScalarView(search.unicodeScalars.prefix(80))))]
        let rooms = try await send("/v1/rooms", query: query, headers: caller.headers(), as: OrganizationRoomsResponse.self).rooms
        guard rooms.count <= 50, Set(rooms.map(\.id)).count == rooms.count,
              rooms.allSatisfy({ $0.memberCount >= 0 && (try? OrganizationRoomInput.identifier($0.id)) != nil }) else {
            throw APIError(status: 200, code: "bad_response", detail: "The room list could not be confirmed.")
        }
        return rooms
    }

    func createOrganizationRoom(_ caller: GroupCaller, payload: OrganizationRoomCreatePayload) async throws -> GroupSummary {
        let data = try await sendRaw("/v1/groups", method: "POST", body: payload.data,
                                     contentType: "application/json", headers: caller.headers())
        let created = try Self.decode(CreatedGroup.self, from: data).group
        guard (try? OrganizationRoomInput.identifier(created.id)) != nil, created.orgId == payload.organizationId,
              created.createdBy == caller.mailbox, created.my?.role == "owner",
              created.my?.state == "keying" || created.my?.state == "active" else {
            throw APIError(status: 200, code: "bad_response", detail: "The room's organization or membership could not be confirmed.")
        }
        return created
    }
}
