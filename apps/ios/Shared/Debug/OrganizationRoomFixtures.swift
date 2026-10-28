#if DEBUG
import Foundation

/// Synthetic transport only. The real authorization generation still owns every screen request.
@MainActor enum OrganizationRoomFixtures {
    private static let session = OrganizationRoomFixtureSession()
    static var enabled: Bool { Fixtures.enabled && CommandLine.arguments.contains("-room-fixtures") }
    fileprivate static let organizationId = "org_room_fixture"
    fileprivate static var memberMailbox = ""
    fileprivate static var memberHandle: String?
    fileprivate static var rooms: [String: [String: Any]] = [:]
    fileprivate static var confirmed: [String: GroupSummary] = [:]
    fileprivate static var creations: [String: String] = [:]
    fileprivate static var lostCreate = false
    private static var configured = false

    static func store(account: Account) -> OrganizationRoomStore {
        guard enabled else {
            return OrganizationRoomStore(session: account.session, mailbox: { account.me?.address },
                                         enabled: { account.features.orgs && account.features.groups })
        }
        session.source = account.session
        if configured {
            return OrganizationRoomStore(session: session, mailbox: { account.me?.address },
                enabled: { account.features.orgs && account.features.groups }, base: URL(string: "https://room-fixture.invalid")!)
        }
        configured = true
        let other = CommandLine.arguments.contains("-room-other-inbox")
        if other, account.mailboxes.count < 2, let me = account.me {
            let extra = Mailbox(address: "/k/fixture-room-member", handle: "/example/member", label: "Example inbox")
            account.installFixtures(mailboxes: account.mailboxes + [extra], me: me)
        }
        let member = other ? account.mailboxes.first { $0.address != account.me?.address } : account.me
        memberMailbox = member?.address ?? "/k/fixture-room-member"; memberHandle = member?.handle
        confirmed = [:]; creations = [:]; lostCreate = false
        rooms = [
            "grp_room_public": ["name": "Design room", "description": "Plans and design reviews", "join_policy": "public", "my_state": NSNull()],
            "grp_room_private": ["name": "Hiring room", "description": "Interview planning", "join_policy": "private", "my_state": "invited"],
            "grp_room_inviteonly": ["name": "Finance room", "description": "Organization accounts", "join_policy": "private", "my_state": NSNull()],
        ]
        if Fixtures.marketing { rooms = MarketingFixtures.rooms }
        URLProtocol.registerClass(OrganizationRoomFixtureTransport.self)
        return OrganizationRoomStore(session: session, mailbox: { account.me?.address },
            enabled: { account.features.orgs && account.features.groups }, base: URL(string: "https://room-fixture.invalid")!)
    }

    static func stage(_ opening: OrganizationRoomOpening, in groups: GroupStore) {
        guard enabled, opening.organizationId == organizationId, opening.scope.mailbox == memberMailbox,
              let group = confirmed[opening.groupId] else { return }
        groups.stageOrganizationRoomFixture(group)
    }

    fileprivate static func group(_ id: String) -> [String: Any]? {
        guard let room = rooms[id] else { return nil }
        return ["id": id, "name": room["name"]!, "description": room["description"] ?? NSNull(),
                "org_id": organizationId, "created_by": memberMailbox, "activation_state": "active",
                "join_policy": room["join_policy"] ?? "private", "post_policy": "everyone", "visibility": "listed",
                "member_count": 4, "revision": 1, "epoch": 1,
                "my": ["role": id.hasPrefix("grp_room_created") ? "owner" : "member", "state": room["my_state"] ?? NSNull()]]
    }
}

@MainActor private final class OrganizationRoomFixtureSession: OrganizationRoomSession {
    weak var source: Session?
    private let fallback = UUID()
    var subject: String? { roomsSignedIn ? source?.subject ?? "fixture-member" : nil }
    var authorizationId: UUID { source?.authorizationId ?? fallback }
    var roomsSignedIn: Bool { source?.status == .signedIn }
    func token() async throws -> String { "synthetic-room-member" }
    func renew() async throws -> String { "synthetic-room-member" }
}

private final class OrganizationRoomFixtureTransport: URLProtocol {
    override class func canInit(with request: URLRequest) -> Bool { request.url?.host == "room-fixture.invalid" }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func stopLoading() {}
    override func startLoading() {
        var bytes = request.httpBody ?? Data()
        if bytes.isEmpty, let stream = request.httpBodyStream {
            stream.open(); defer { stream.close() }
            var buffer = [UInt8](repeating: 0, count: 2048)
            while stream.hasBytesAvailable {
                let count = stream.read(&buffer, maxLength: buffer.count)
                if count <= 0 { break }; bytes.append(contentsOf: buffer.prefix(count))
            }
        }
        Task { @MainActor in
            guard OrganizationRoomFixtures.enabled else { client?.urlProtocol(self, didFailWithError: URLError(.unsupportedURL)); return }
            let body = (try? JSONSerialization.jsonObject(with: bytes)) as? [String: Any] ?? [:]
            let path = request.url!.path
            let query = URLComponents(url: request.url!, resolvingAgainstBaseURL: false)?.queryItems ?? []
            let caller = request.value(forHTTPHeaderField: "x-pigeonpost-identity") ?? ""
            var status = 200, answer: [String: Any] = [:]
            if path == "/v1/orgs" {
                let readonly = CommandLine.arguments.contains("-room-read-only")
                answer = ["orgs": [["id": OrganizationRoomFixtures.organizationId, "name": Fixtures.marketing ? MarketingFixtures.organizationName : "Example Studio", "namespace": Fixtures.marketing ? MarketingFixtures.organizationNamespace : "example",
                    "status": readonly ? "read_only" : "active", "my_roles": [], "settings": ["rooms_creation": "everyone"],
                    "my_mailbox": ["address": OrganizationRoomFixtures.memberMailbox, "handle": OrganizationRoomFixtures.memberHandle as Any? ?? NSNull()]]], "operated": []]
            } else if caller != OrganizationRoomFixtures.memberMailbox {
                status = 403; answer = ["error": "not_org_member"]
            } else if path == "/v1/rooms" {
                if query.first(where: { $0.name == "org_id" })?.value != OrganizationRoomFixtures.organizationId {
                    status = 400; answer = ["error": "missing_org"]
                } else {
                    let search = query.first(where: { $0.name == "q" })?.value ?? ""
                    answer = ["rooms": OrganizationRoomFixtures.rooms.keys.sorted().compactMap { id -> [String: Any]? in
                        guard let room = OrganizationRoomFixtures.rooms[id], let name = room["name"] as? String,
                              search.isEmpty || name.localizedCaseInsensitiveContains(search) else { return nil }
                        return ["id": id, "name": name, "description": room["description"] ?? NSNull(), "member_count": Fixtures.marketing ? (["grp_room_launch": 9, "grp_room_reviews": 6, "grp_room_support": 12, "grp_room_leads": 5][id] ?? 4) : 4,
                                "my_state": room["my_state"] ?? NSNull(), "join_policy": room["join_policy"] ?? "private"]
                    }]
                }
            } else if path == "/v1/groups/resolve", let id = body["group_id"] as? String, let room = OrganizationRoomFixtures.rooms[id] {
                let state = room["my_state"] as? String
                answer = ["id": id, "name": room["name"]!, "description": room["description"] ?? NSNull(), "member_count": 4,
                          "my_state": state as Any? ?? NSNull(), "join_policy": room["join_policy"] ?? "private",
                          "can_join": room["join_policy"] as? String == "public" && !["active", "keying", "banned"].contains(state ?? ""),
                          "needs": state == "invited" ? "invitation" : "join"]
            } else if path == "/v1/groups", request.httpMethod == "POST", body["org_id"] as? String == OrganizationRoomFixtures.organizationId,
                      let key = body["request_key"] as? String {
                let id = OrganizationRoomFixtures.creations[key] ?? "grp_room_created_\(OrganizationRoomFixtures.creations.count + 1)"
                OrganizationRoomFixtures.creations[key] = id
                OrganizationRoomFixtures.rooms[id] = ["name": body["name"] ?? "New room", "description": body["description"] ?? NSNull(),
                    "join_policy": body["join_policy"] ?? "public", "my_state": "keying"]
                if CommandLine.arguments.contains("-room-lost-create"), !OrganizationRoomFixtures.lostCreate {
                    OrganizationRoomFixtures.lostCreate = true
                    client?.urlProtocol(self, didFailWithError: URLError(.networkConnectionLost)); return
                }
                status = 201; answer = ["group": OrganizationRoomFixtures.group(id)!]
            } else if path.hasPrefix("/v1/groups/") {
                let parts = path.split(separator: "/")
                let id = parts.count >= 3 ? String(parts[2]) : ""
                if OrganizationRoomFixtures.rooms[id] != nil {
                    if request.httpMethod == "POST", path.hasSuffix("/join") || path.hasSuffix("/invitation") {
                        OrganizationRoomFixtures.rooms[id]?["my_state"] = "keying"
                        answer = ["group": OrganizationRoomFixtures.group(id)!]
                    } else if request.httpMethod == "GET", let group = OrganizationRoomFixtures.group(id) {
                        answer = group
                        if let data = try? JSONSerialization.data(withJSONObject: group), let decoded = try? PostboxClient.decode(GroupSummary.self, from: data) {
                            OrganizationRoomFixtures.confirmed[id] = decoded
                        }
                    } else { status = 404; answer = ["error": "not_found"] }
                } else { status = 404; answer = ["error": "not_found"] }
            } else { status = 404; answer = ["error": "not_found"] }
            let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil, headerFields: ["content-type": "application/json"])!
            client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
            client?.urlProtocol(self, didLoad: (try? JSONSerialization.data(withJSONObject: answer)) ?? Data())
            client?.urlProtocolDidFinishLoading(self)
        }
    }
}
#endif
