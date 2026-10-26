import Foundation
import Observation

@MainActor protocol OrganizationRoomSession: TokenProviding {
    var subject: String? { get }
    var authorizationId: UUID { get }
    var roomsSignedIn: Bool { get }
}

extension Session: OrganizationRoomSession {
    var roomsSignedIn: Bool { status == .signedIn }
}

struct OrganizationRoomScope: Equatable, Hashable {
    let subject: String
    let authorization: UUID
    let mailbox: String
}

struct OrganizationRoomOpening: Equatable {
    let scope: OrganizationRoomScope
    let organizationId: String
    let groupId: String
}

@MainActor private final class OrganizationRoomTokens: TokenProviding {
    private weak var source: TokenProviding?
    private let current: () -> Bool
    init(source: TokenProviding, current: @escaping () -> Bool) { self.source = source; self.current = current }
    func token() async throws -> String {
        guard current(), !Task.isCancelled, let source else { throw CancellationError() }
        let value = try await source.token()
        guard current(), !Task.isCancelled else { throw CancellationError() }
        return value
    }
    func renew() async throws -> String {
        guard current(), !Task.isCancelled, let source else { throw CancellationError() }
        let value = try await source.renew()
        guard current(), !Task.isCancelled else { throw CancellationError() }
        return value
    }
}

/// Directory state is scoped independently of the group's decrypted conversation cache.
@MainActor @Observable final class OrganizationRoomStore {
    @ObservationIgnored private weak var session: (any OrganizationRoomSession)?
    @ObservationIgnored private let actingMailbox: () -> String?
    @ObservationIgnored private let enabled: () -> Bool
    @ObservationIgnored private let base: URL
    private var scope: OrganizationRoomScope?
    private var generation = UUID()
    private var catalogRead = UUID()
    private var roomRead = UUID()
    private var reviewRead = UUID()
    private var mutation = UUID()
    private var catalog: RoomOrganizationsResponse?
    private var roomValues: [OrganizationRoom] = []
    private var reviewValue: GroupPreview?
    private var reviewOrganization: String?
    private var createValue: OrganizationRoomCreatePayload?
    private var errorValue: String?
    private(set) var selectedOrganizationId: String?
    private var loadingOrganizations = false
    private var loadingRooms = false
    var loading: Bool { contextMatches && (loadingOrganizations || loadingRooms) }
    private(set) var reviewing = false
    private(set) var busy = false
    private(set) var reviewProof = UUID()
    private(set) var mustReviewAgain = false
    private(set) var retryAt: Date?

    init(session: any OrganizationRoomSession, mailbox: @escaping () -> String?,
         enabled: @escaping () -> Bool, base: URL = Config.postbox) {
        self.session = session; actingMailbox = mailbox; self.enabled = enabled; self.base = base
    }

    var context: OrganizationRoomScope? {
        guard enabled(), let session, session.roomsSignedIn,
              let subject = session.subject, !subject.isEmpty,
              let mailbox = actingMailbox(), !mailbox.isEmpty else { return nil }
        return .init(subject: subject, authorization: session.authorizationId, mailbox: mailbox)
    }
    private var contextMatches: Bool { scope != nil && scope == context }
    var organizations: [RoomOrganization] { contextMatches ? catalog?.orgs ?? [] : [] }
    var matchingOrganizations: [RoomOrganization] {
        guard contextMatches, let scope else { return [] }
        return catalog?.organizations(for: scope.mailbox) ?? []
    }
    var organization: RoomOrganization? { matchingOrganizations.first { $0.id == selectedOrganizationId } }
    var rooms: [OrganizationRoom] { contextMatches && organization?.readable == true ? roomValues : [] }
    var preview: GroupPreview? { contextMatches && reviewOrganization == selectedOrganizationId ? reviewValue : nil }
    var pendingCreation: OrganizationRoomCreatePayload? {
        contextMatches && createValue?.organizationId == organization?.id ? createValue : nil
    }
    var error: String? { contextMatches ? errorValue : nil }
    var canCreate: Bool { contextMatches && scope.map { organization?.mayCreate(as: $0.mailbox) == true } == true && !busy }
    var reviewAction: OrganizationRoom.Action {
        guard !mustReviewAgain, let preview else { return .unavailable }
        let action: OrganizationRoom.Action
        switch preview.myState {
        case "active", "keying": action = .open
        case "invited": action = .acceptInvitation
        case nil, "left", "removed": action = preview.joinPolicy == "public" && preview.canJoin == true ? .join : .unavailable
        default: action = .unavailable
        }
        return action == .open || organization?.writable == true ? action : .unavailable
    }

    func synchronize() {
        let next = context
        guard next != scope else { return }
        let deadline = scope?.subject == next?.subject && scope?.authorization == next?.authorization ? retryAt : nil
        scope = next; generation = UUID(); catalogRead = UUID(); roomRead = UUID(); reviewRead = UUID(); mutation = UUID()
        catalog = nil; roomValues = []; selectedOrganizationId = nil; createValue = nil; errorValue = nil
        loadingOrganizations = false; loadingRooms = false; reviewing = false; busy = false; retryAt = deadline
        closeReview()
    }

    func closeReview() {
        reviewRead = UUID(); reviewProof = UUID(); reviewValue = nil; reviewOrganization = nil
        reviewing = false; mustReviewAgain = false
    }

    @discardableResult func selectOrganization(_ id: String) -> Bool {
        synchronize()
        guard !busy, matchingOrganizations.contains(where: { $0.id == id }) else { return false }
        guard selectedOrganizationId != id else { return true }
        guard createValue == nil else { errorValue = "Check or discard the previous creation attempt before choosing another organization."; return false }
        selectedOrganizationId = id; roomRead = UUID(); roomValues = []; createValue = nil; errorValue = nil
        closeReview()
        return true
    }

    func loadOrganizations(search: String = "") async {
        synchronize()
        guard let scope, let session else { return }
        let visit = generation, read = UUID(); catalogRead = read; loadingOrganizations = true; errorValue = nil
        let tokens = OrganizationRoomTokens(source: session) { [weak self] in
            self?.current(scope, visit) == true && self?.catalogRead == read
        }
        defer { withExtendedLifetime(tokens) {}; if owns(scope, visit), catalogRead == read { loadingOrganizations = false } }
        let client = PostboxClient(base: base, tokens: tokens)
        do {
            let response = try await client.roomOrganizations(.init(mailbox: scope.mailbox))
            guard current(scope, visit), catalogRead == read else { synchronize(); return }
            catalog = response
            let matches = response.organizations(for: scope.mailbox)
            let before = selectedOrganizationId
            if !matches.contains(where: { $0.id == selectedOrganizationId }) {
                selectedOrganizationId = matches.count == 1 ? matches[0].id : nil
            }
            if before != selectedOrganizationId { createValue = nil }
            if before != selectedOrganizationId || organization?.readable != true { roomValues = []; closeReview() }
            await loadRooms(search: search)
        } catch {
            guard current(scope, visit), catalogRead == read else { synchronize(); return }
            report(error, fallback: "Your organizations could not be loaded. Try again.")
        }
    }

    func loadRooms(search: String = "") async {
        synchronize()
        guard let scope, let organization, organization.readable, let session else { return }
        let org = organization.id, visit = generation, read = UUID(); roomRead = read; loadingRooms = true; errorValue = nil
        let tokens = OrganizationRoomTokens(source: session) { [weak self] in
            self?.current(scope, visit, organization: org) == true && self?.roomRead == read
        }
        defer { withExtendedLifetime(tokens) {}; if owns(scope, visit), roomRead == read { loadingRooms = false } }
        let client = PostboxClient(base: base, tokens: tokens)
        do {
            let result = try await client.organizationRooms(.init(mailbox: scope.mailbox), organizationId: org, search: search)
            guard current(scope, visit, organization: org), roomRead == read else { synchronize(); return }
            roomValues = result
        } catch {
            guard current(scope, visit, organization: org), roomRead == read else { synchronize(); return }
            report(error, fallback: "The rooms could not be loaded. Try again.")
        }
    }

    func inspect(_ id: String) async {
        synchronize()
        guard !busy, let scope, let organization, organization.readable, roomValues.contains(where: { $0.id == id }), let session else { return }
        closeReview(); errorValue = nil; reviewing = true
        let org = organization.id, visit = generation, read = reviewRead
        let tokens = OrganizationRoomTokens(source: session) { [weak self] in
            self?.current(scope, visit, organization: org) == true && self?.reviewRead == read
        }
        defer { withExtendedLifetime(tokens) {}; if owns(scope, visit), reviewRead == read { reviewing = false } }
        let client = PostboxClient(base: base, tokens: tokens)
        do {
            let result = try await client.resolveGroup(.init(mailbox: scope.mailbox), id: id, linkToken: nil)
            guard current(scope, visit, organization: org), reviewRead == read else { synchronize(); return }
            guard result.id == id else { throw APIError(status: 200, code: "bad_response", detail: nil) }
            reviewValue = result; reviewOrganization = org; reviewProof = UUID()
        } catch {
            guard current(scope, visit, organization: org), reviewRead == read else { synchronize(); return }
            report(error, fallback: "The room could not be confirmed. Refresh and try again.")
        }
    }

    func openReviewedRoom(proof: UUID) async -> OrganizationRoomOpening? {
        synchronize()
        guard !busy, proof == reviewProof, let scope, let organization, let preview, let session else { return nil }
        let action = reviewAction
        guard action != .unavailable else { return nil }
        let org = organization.id, visit = generation, operation = UUID(), id = preview.id
        mutation = operation; busy = true; errorValue = nil
        let tokens = OrganizationRoomTokens(source: session) { [weak self] in
            self?.current(scope, visit, organization: org) == true && self?.mutation == operation && self?.reviewProof == proof
        }
        defer { withExtendedLifetime(tokens) {}; if mutation == operation { busy = false } }
        let client = PostboxClient(base: base, tokens: tokens), caller = GroupCaller(mailbox: scope.mailbox)
        do {
            do {
                if action == .join { try await client.joinGroup(caller, id: id, linkToken: nil) }
                if action == .acceptInvitation { try await client.answerInvitation(caller, id: id, accept: true) }
            } catch let error as APIError where error.code == "already_member" {
                // Another device may already have joined; confirm live membership below.
            }
            let group = try await client.group(caller, id: id)
            guard current(scope, visit, organization: org), mutation == operation, reviewProof == proof else { synchronize(); return nil }
            try confirm(group, id: id, organization: org)
            return .init(scope: scope, organizationId: org, groupId: id)
        } catch {
            guard current(scope, visit, organization: org), mutation == operation else { synchronize(); return nil }
            mustReviewAgain = true
            report(error, fallback: "The result could not be confirmed. Check the room before trying again.")
            return nil
        }
    }

    func createRoom(in expectedScope: OrganizationRoomScope, organizationId: String, name: String, description: String, joinPolicy: OrganizationRoomJoinPolicy,
                    postPolicy: OrganizationRoomPostPolicy, listed: Bool) async -> OrganizationRoomOpening? {
        synchronize()
        guard expectedScope == context, canCreate, let scope, let organization, organization.id == organizationId, let session else { return nil }
        if let retryAt, retryAt > Date() { errorValue = "Wait before retrying this request."; return nil }
        let payload: OrganizationRoomCreatePayload
        do {
            payload = try .init(organizationId: organization.id, name: name, description: description,
                joinPolicy: joinPolicy, postPolicy: postPolicy, listed: listed,
                requestKey: createValue?.requestKey ?? "rk_" + UUID().uuidString.lowercased())
            guard createValue == nil || createValue == payload else {
                errorValue = "Confirm or discard the previous creation attempt before changing its details."; return nil
            }
        } catch { errorValue = (error as? APIError)?.errorDescription ?? "Check the room details."; return nil }
        createValue = payload
        let org = organization.id, visit = generation, operation = UUID(); mutation = operation; busy = true; errorValue = nil
        let tokens = OrganizationRoomTokens(source: session) { [weak self] in
            self?.current(scope, visit, organization: org) == true && self?.mutation == operation
        }
        defer { withExtendedLifetime(tokens) {}; if mutation == operation { busy = false } }
        let client = PostboxClient(base: base, tokens: tokens), caller = GroupCaller(mailbox: scope.mailbox)
        do {
            let created = try await client.createOrganizationRoom(caller, payload: payload)
            let currentGroup = try await client.group(caller, id: created.id)
            guard current(scope, visit, organization: org), mutation == operation else { synchronize(); return nil }
            try confirm(currentGroup, id: created.id, organization: org)
            createValue = nil; retryAt = nil
            return .init(scope: scope, organizationId: org, groupId: created.id)
        } catch {
            guard current(scope, visit, organization: org), mutation == operation else { synchronize(); return nil }
            if let api = error as? APIError {
                if (400..<500).contains(api.status), ![408, 409, 429].contains(api.status) { createValue = nil }
                if api.status == 429 { retryAt = Date().addingTimeInterval(TimeInterval(max(1, min(api.retryAfter ?? 30, 3600)))) }
            }
            report(error, fallback: "Creation could not be confirmed. Retry the same details or check your existing rooms.")
            return nil
        }
    }

    /// The UI must explain that an uncertain attempt may already have created a room.
    func discardCreation(in expectedScope: OrganizationRoomScope, organizationId: String) {
        guard !busy, expectedScope == context, organization?.id == organizationId else { return }
        createValue = nil; errorValue = nil
    }

    func allows(_ opening: OrganizationRoomOpening) -> Bool {
        contextMatches && context == opening.scope && organization?.id == opening.organizationId && organization?.readable == true
    }

    private func current(_ scope: OrganizationRoomScope, _ visit: UUID, organization org: String? = nil) -> Bool {
        guard owns(scope, visit), !Task.isCancelled else { return false }
        return org == nil || (organization?.id == org && organization?.readable == true)
    }

    private func owns(_ scope: OrganizationRoomScope, _ visit: UUID) -> Bool {
        self.scope == scope && context == scope && generation == visit
    }

    private func confirm(_ group: GroupSummary, id: String, organization: String) throws {
        guard group.id == id, group.orgId == organization, group.deletedAt == nil,
              group.my?.state == "active" || group.my?.state == "keying" else {
            throw APIError(status: 200, code: "bad_response", detail: "Room membership could not be confirmed.")
        }
    }

    private func report(_ error: Error, fallback: String) {
        if error is CancellationError { return }
        if let api = error as? APIError, [401, 403, 404, 410].contains(api.status) {
            catalog = nil; roomValues = []; selectedOrganizationId = nil; createValue = nil; retryAt = nil; closeReview()
            errorValue = "Access could not be confirmed. Refresh your organizations."
        } else { errorValue = fallback }
    }
}
