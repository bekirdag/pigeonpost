import Foundation

enum AuthError: Error { case sessionExpired }
@MainActor final class Session {
    var renewals = 0
    func token() async throws -> String { renewals == 0 ? "fixture-before" : "fixture-after" }
    func renew() async throws -> String { renewals += 1; return "fixture-after" }
}

private final class RoomTransport: URLProtocol, @unchecked Sendable {
    struct Reply { let status: Int; let data: Data }
    @MainActor static var replies: [Reply] = []
    @MainActor static var requests: [URLRequest] = []
    @MainActor static var bodies: [Data] = []
    override class func canInit(with request: URLRequest) -> Bool { request.url?.host == "rooms.invalid" }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}
    override func startLoading() {
        Task { @MainActor in
            Self.requests.append(request)
            var data = request.httpBody ?? Data()
            if data.isEmpty, let stream = request.httpBodyStream {
                stream.open(); defer { stream.close() }
                var buffer = [UInt8](repeating: 0, count: 2048)
                while stream.hasBytesAvailable {
                    let count = stream.read(&buffer, maxLength: buffer.count)
                    if count <= 0 { break }; data.append(contentsOf: buffer.prefix(count))
                }
            }
            Self.bodies.append(data)
            guard !Self.replies.isEmpty else { client?.urlProtocol(self, didFailWithError: URLError(.badServerResponse)); return }
            let reply = Self.replies.removeFirst()
            let response = HTTPURLResponse(url: request.url!, statusCode: reply.status, httpVersion: nil, headerFields: ["content-type": "application/json"])!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: reply.data)
            client?.urlProtocolDidFinishLoading(self)
        }
    }
}

@main enum OrganizationRoomTests {
    @MainActor static func main() async throws {
        var checks = 0
        func check(_ value: Bool, _ name: String) { precondition(value, name); checks += 1 }
        func data(_ object: [String: Any]) throws -> Data { try JSONSerialization.data(withJSONObject: object) }
        func organization(status: String = "active", policy: String? = "everyone", roles: [String] = [], mailbox: String? = "/k/member") -> RoomOrganization {
            RoomOrganization(id: "org_fixture", name: "Example", namespace: "example", status: status,
                settings: .init(roomsCreation: policy), myRoles: roles, myMemberId: "member_fixture",
                myMailbox: mailbox.map { .init(address: $0, handle: "/example/person") })
        }
        let org = organization()
        check(org.mayCreate(as: "/k/member"), "everyone policy applies to a live human member's own inbox")
        check(!org.mayCreate(as: "/example/person"), "a handle resemblance is not membership")
        check(!org.mayCreate(as: "/k/agent"), "operating an agent cannot impersonate the person's inbox")
        check(!organization(policy: "admins").mayCreate(as: "/k/member"), "an ordinary member cannot create under an admin policy")
        check(organization(policy: "admins", roles: ["billing_admin"]).mayCreate(as: "/k/member"), "server CreateRoom policy accepts an explicit organization role")
        check(!organization(policy: nil, roles: ["owner"]).mayCreate(as: "/k/member"), "unknown policy grants no UI permission")
        check(!organization(mailbox: nil).mayCreate(as: "/k/member"), "a role without an org inbox cannot create a room")
        check(organization(status: "past_due").mayCreate(as: "/k/member"), "past-due behavior matches the current server contract")
        for status in ["read_only", "suspended", "future_status"] {
            check(!organization(status: status).mayCreate(as: "/k/member"), "non-writing organization cannot create")
        }
        check(organization(status: "read_only").readable && !organization(status: "suspended").readable, "read and write policies are distinct")
        let memberships = RoomOrganizationsResponse(orgs: [org], operated: [
            .init(address: "/k/agent", handle: "/example/agent", orgId: org.id, capabilities: ["read_inbox"]),
            .init(address: "/k/configurer", handle: nil, orgId: org.id, capabilities: ["configure_rules"]),
            .init(address: "/k/foreign", handle: nil, orgId: "org_other", capabilities: ["read_inbox"]),
        ])
        check(memberships.organizations(for: "/k/member").map(\.id) == [org.id], "canonical member association")
        check(memberships.organizations(for: "/k/agent").map(\.id) == [org.id], "explicit read-inbox delegation associates the agent")
        check(memberships.organizations(for: "/k/configurer").isEmpty, "a different delegation grants no inbox access")
        check(memberships.organizations(for: "/k/foreign").isEmpty, "delegations do not add an invisible organization")
        check(memberships.organizations(for: "/example/person").isEmpty, "namespace spelling cannot associate a mailbox")
        for state: String? in [nil, "left", "removed", "banned", "future_state"] {
            let room = OrganizationRoom(id: "g_fixture", name: "Room", description: nil, memberCount: 2, myState: state, joinPolicy: "public")
            check(room.action != .open, "missing or non-member status never opens a room")
            if state == "banned" || state == "future_state" { check(room.action == .unavailable, "unknown or banned membership cannot join") }
        }
        for state in ["active", "keying"] {
            check(OrganizationRoom(id: "g_fixture", name: "Room", description: nil, memberCount: 2, myState: state, joinPolicy: "private").action == .open, "explicit membership can open encryption status")
        }
        check(OrganizationRoom(id: "g_fixture", name: "Room", description: nil, memberCount: 2, myState: "invited", joinPolicy: "private").action == .acceptInvitation, "private invitations require acceptance")
        check(OrganizationRoom(id: "g_fixture", name: "Room", description: nil, memberCount: 2, myState: nil, joinPolicy: "private").action == .unavailable, "a listed private room grants no join permission")

        func payload(name: String = "Design", description: String = "Notes", org: String = "org_fixture") throws -> OrganizationRoomCreatePayload {
            try .init(organizationId: org, name: name, description: description, joinPolicy: .members, postPolicy: .everyone,
                      listed: true, requestKey: "rk_fixture_room_001")
        }
        let create = try payload()
        let fields = try JSONSerialization.jsonObject(with: create.data) as! [String: Any]
        check(fields["org_id"] as? String == org.id && fields["request_key"] as? String == "rk_fixture_room_001", "creation is organization-bound and idempotent")
        check(fields["join_policy"] as? String == "public" && fields["post_policy"] as? String == "everyone", "wire policies match the server")
        check(fields["visibility"] as? String == "listed" && fields["history_visible_to_new_members"] as? Bool == false, "listing shares no prior encrypted history")
        check(try payload(name: "  Design  ", description: "  Notes  ").data == create.data, "equivalent drafts preserve canonical bytes")
        check(try payload(name: String(repeating: "ö", count: 80)).name.unicodeScalars.count == 80, "names use the server's Unicode scalar bound")
        for bad in ["", " ", "Line\nbreak", "nul\0name", String(repeating: "a", count: 81), String(repeating: "a\u{301}", count: 41)] {
            check((try? payload(name: bad)) == nil, "invalid names are rejected before network work")
        }
        check((try? payload(description: String(repeating: "x", count: 501))) == nil, "description is bounded")
        for bad in ["", "../org", "org/other", "org?x=y", "org#x", "org%2fother", "ö", String(repeating: "a", count: 129)] {
            check((try? payload(org: bad)) == nil, "invalid org identifiers cannot normalize into a personal route")
        }

        URLProtocol.registerClass(RoomTransport.self)
        defer { URLProtocol.unregisterClass(RoomTransport.self) }
        let tokens = Session()
        let client = PostboxClient(base: URL(string: "https://rooms.invalid")!, tokens: tokens)
        let caller = GroupCaller(mailbox: "/k/member")
        let orgWire: [String: Any] = ["id": org.id, "name": org.name, "namespace": "example", "status": "active", "settings": ["rooms_creation": "everyone"], "my_roles": [], "my_member_id": "member_fixture", "my_mailbox": ["address": "/k/member", "handle": "/example/person"]]
        RoomTransport.replies = [.init(status: 200, data: try data(["orgs": [orgWire], "operated": []]))]
        let listed = try await client.roomOrganizations(caller)
        check(listed.orgs == [org], "real shared-client organization decoding")
        check(RoomTransport.requests.last?.url?.path == "/v1/orgs" && RoomTransport.requests.last?.value(forHTTPHeaderField: "x-pigeonpost-identity") == caller.mailbox, "organization discovery is bound to the acting context")
        let roomWire: [String: Any] = ["id": "g_fixture", "name": "Room", "description": NSNull(), "member_count": 2, "my_state": NSNull(), "join_policy": "public"]
        RoomTransport.replies = [.init(status: 200, data: try data(["rooms": [roomWire]]))]
        let rooms = try await client.organizationRooms(caller, organizationId: org.id, search: "Room & üyeler?")
        let request = RoomTransport.requests.last!
        let query = Dictionary(uniqueKeysWithValues: URLComponents(url: request.url!, resolvingAgainstBaseURL: false)!.queryItems!.map { ($0.name, $0.value ?? "") })
        check(rooms.count == 1 && rooms[0].action == .join, "directory decoding does not invent membership")
        check(request.httpMethod == "GET" && request.url?.path == "/v1/rooms" && query["org_id"] == org.id && query["q"] == "Room & üyeler?", "organization and search are always explicit query parameters")
        let before = RoomTransport.requests.count
        do { _ = try await client.organizationRooms(caller, organizationId: "", search: ""); preconditionFailure("missing organization accepted") }
        catch let error as APIError { check(error.code == "room_input", "absent organization is a local refusal") }
        check(RoomTransport.requests.count == before, "there is no personal-directory fallback request")
        let group: [String: Any] = ["id": "g_created", "name": "Design", "org_id": org.id, "created_by": caller.mailbox, "activation_state": "provisioning", "my": ["role": "owner", "state": "keying"]]
        RoomTransport.requests = []; RoomTransport.bodies = []
        RoomTransport.replies = [.init(status: 401, data: try data(["error": "unauthorized"])), .init(status: 201, data: try data(["group": group]))]
        let created = try await client.createOrganizationRoom(caller, payload: create)
        check(created.orgId == org.id && created.my?.state == "keying", "created room retains organization and actual encryption membership")
        check(tokens.renewals == 1 && RoomTransport.requests.count == 2, "ordinary credential expiry renews once")
        check(RoomTransport.bodies.allSatisfy { $0 == create.data }, "renewal preserves the exact create bytes and idempotency key")
        check(RoomTransport.requests.allSatisfy { $0.httpMethod == "POST" && $0.url?.path == "/v1/groups" && $0.url?.query == nil && $0.value(forHTTPHeaderField: "x-pigeonpost-identity") == caller.mailbox }, "renewal preserves the acting mailbox and fixed create endpoint")
        check(RoomTransport.requests.first?.value(forHTTPHeaderField: "authorization") == "Bearer fixture-before" && RoomTransport.requests.last?.value(forHTTPHeaderField: "authorization") == "Bearer fixture-after", "only authorization changes on ordinary renewal")
        for bad in [group.merging(["org_id": "org_other"]) { _, n in n }, group.merging(["my": NSNull()]) { _, n in n }, group.merging(["created_by": "/k/other"]) { _, n in n }, group.merging(["id": "../other"]) { _, n in n }] {
            RoomTransport.replies = [.init(status: 201, data: try data(["group": bad]))]
            do { _ = try await client.createOrganizationRoom(caller, payload: create); preconditionFailure("unconfirmed create accepted") }
            catch let error as APIError { check(error.code == "bad_response", "a create cannot silently change organization, actor or membership") }
        }
        RoomTransport.replies = [.init(status: 200, data: try data(["orgs": [orgWire, orgWire]]))]
        do { _ = try await client.roomOrganizations(caller); preconditionFailure("duplicate orgs accepted") }
        catch let error as APIError { check(error.code == "bad_response", "ambiguous organization lists are refused") }
        for badRooms in [[roomWire, roomWire], [roomWire.merging(["id": "../g_other"]) { _, n in n }], [roomWire.merging(["member_count": -1]) { _, n in n }]] {
            RoomTransport.replies = [.init(status: 200, data: try data(["rooms": badRooms]))]
            do { _ = try await client.organizationRooms(caller, organizationId: org.id, search: ""); preconditionFailure("invalid room list accepted") }
            catch let error as APIError { check(error.code == "bad_response", "malformed room metadata cannot become selectable") }
        }
        check(RoomTransport.requests.allSatisfy { $0.url?.host == "rooms.invalid" }, "wire checks stay on the isolated fixture transport")
        print("Organization rooms model and wire: \(checks) checks passed")
    }
}
