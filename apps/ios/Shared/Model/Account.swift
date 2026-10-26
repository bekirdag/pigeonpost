//  The account's mailboxes, and which one the app is acting as.
//
//  `/v1/identities` reports an address and the operator's own label. The handle — the thing trust
//  actually matches on — is only knowable from the server, one mailbox at a time, so the two calls
//  are made together and the result is what the rest of the app calls a Mailbox.

import Foundation
import Observation

@MainActor
@Observable
final class Account {
    enum Load: Equatable { case idle, loading, ready, failed(String) }

    private var cachedMailboxes: [Mailbox] = []
    private var cachedMe: Mailbox?
    private var cachedLoad: Load = .idle
    private var cachedAuthorization: UUID?
    private var cachedSubject: String?
    private var loadAuthorization: UUID?
    private var loadRequest: UUID?
    private var creationRequest: UUID?
    private var creatingAuthorization: UUID?
    private var retiringAuthorization: UUID?
    @ObservationIgnored private var actorLease: AuthorizationLease?
    @ObservationIgnored private var actorLeaseAuthorization: UUID?
    private(set) var actingGeneration = UUID()

    var mailboxes: [Mailbox] { hasCurrentOwner ? cachedMailboxes : [] }
    var me: Mailbox? { hasCurrentOwner ? cachedMe : nil }
    var load: Load { hasCurrentOwner || loadAuthorization == session.authorizationId ? cachedLoad : .idle }
    /// A first mailbox is being minted. Its own flag rather than `load`, because `loadIdentities`
    /// declines to run while `load` is `.loading` and creating ends by calling it.
    var creating: Bool { creationRequest != nil && creatingAuthorization == session.authorizationId && session.status == .signedIn }

    let session: Session
    /// Long-lived services bind their own lifecycle to this provider.
    let client: PostboxClient
    private let base: URL
    private let defaults: UserDefaults
    /// Snapshot a grant when an operation starts. A retained proxy never retains the session.
    func scopedClient(acting: Bool = false) -> PostboxClient {
        let authorization = session.authorizationId
        let actor = acting ? actingGeneration : nil
        let session = session
        let tokens = ScopedTokenProvider(source: session) { [weak session, weak self] in
            session?.status == .signedIn && session?.authorizationId == authorization
                && (actor == nil || self?.actingGeneration == actor)
        }
        return PostboxClient(base: base, tokens: tokens, retainingTokens: true)
    }
    /// What the postbox has switched on. Account-wide: capabilities are per account, not per mailbox.
    let features = Features()
    @ObservationIgnored private var purchaseStore: HandleStore?

    var handles: HandleStore {
        if let purchaseStore { return purchaseStore }
        let store = HandleStore(account: self)
        purchaseStore = store
        return store
    }

    private let rememberedKey = "ppi_identity"

    init(session: Session, base: URL = Config.postbox, defaults: UserDefaults = .standard) {
        self.session = session
        self.client = PostboxClient(base: base, tokens: session)
        self.base = base
        self.defaults = defaults
        features.visible = { [weak self] in self?.hasCurrentOwner == true }
    }

    /// Same-person strong authentication keeps its screen and capabilities. Another person may
    /// never see this person's identity cache while its new request is still pending.
    private var hasCurrentOwner: Bool {
        #if DEBUG
        if Fixtures.enabled { return true }
        #endif
        guard session.status == .signedIn, retiringAuthorization != session.authorizationId else { return false }
        return cachedAuthorization == session.authorizationId
            || (cachedSubject != nil && cachedSubject == session.subject)
    }

    private func current(_ authorization: UUID) -> Bool {
        session.status == .signedIn && session.authorizationId == authorization
            && retiringAuthorization != authorization && !Task.isCancelled
    }

    /// The synchronous disk boundary follows both the grant and the selected inbox.
    func accessForActingMailbox() -> AuthorizationLease? {
        guard me != nil, current(session.authorizationId) else { return nil }
        if actorLeaseAuthorization != session.authorizationId || actorLease?.isValid != true {
            actorLease?.invalidate()
            actorLease = AuthorizationLease(parent: session.accessLease)
            actorLeaseAuthorization = session.authorizationId
        }
        return actorLease
    }

    private func retireActor() {
        actorLease?.invalidate()
        actorLease = nil
        actorLeaseAuthorization = nil
        actingGeneration = UUID()
    }

    func loadIdentities() async {
        let authorization = session.authorizationId
        guard current(authorization) else { return }
        if loadAuthorization == authorization, case .loading = cachedLoad { return }
        let request = UUID()
        let client = scopedClient()
        if !hasCurrentOwner { features.reset() }
        loadAuthorization = authorization
        loadRequest = request
        cachedLoad = .loading
        defer {
            if loadRequest == request {
                loadRequest = nil
                if case .loading = cachedLoad { cachedLoad = .idle }
            }
        }
        func owns() -> Bool { current(authorization) && loadRequest == request }
        do {
            let rows = try await client.identities()
            guard owns() else { return }
            let resolved = await withTaskGroup(of: (Int, Mailbox).self) { group in
                for (index, row) in rows.enumerated() {
                    group.addTask { [client] in
                        // A mailbox whose handle cannot be read is still a mailbox: it is the
                        // anonymous ones that a fleet is mostly made of.
                        let handle = try? await client.whoami(identity: row.address).handle
                        return (index, Mailbox(address: row.address, handle: handle ?? nil, label: row.label))
                    }
                }
                var collected: [(Int, Mailbox)] = []
                for await result in group { collected.append(result) }
                return collected.sorted { $0.0 < $1.0 }.map(\.1)
            }
            guard owns() else { return }
            let previous = me?.address
            let selected = previous.flatMap { address in resolved.first { $0.address == address } } ?? preferred(among: resolved)
            if previous != selected?.address { retireActor() }
            cachedMailboxes = resolved
            cachedMe = selected
            cachedAuthorization = authorization
            cachedSubject = session.subject
            cachedLoad = .ready
            await features.load(using: client, ifCurrent: owns)
        } catch let error as APIError {
            guard owns() else { return }
            cachedLoad = .failed(error.errorDescription ?? "Could not reach the postbox.")
        } catch is AuthError {
            guard owns() else { return }
            cachedLoad = .failed("Your session expired. Sign in again.")
        } catch {
            guard owns() else { return }
            cachedLoad = .failed("Could not reach the postbox.")
        }
    }

    /// Mint the account's first mailbox, then load it like any other.
    ///
    /// An account holder needs no proof-of-work: the postbox creates under the account on the
    /// strength of the token this app already holds, so what used to be "go and install a command
    /// line tool" is one call.
    func createFirstMailbox() async {
        guard !creating else { return }
        let authorization = session.authorizationId
        guard current(authorization) else { return }
        let request = UUID()
        creationRequest = request
        creatingAuthorization = authorization
        defer { if creationRequest == request { creationRequest = nil; creatingAuthorization = nil } }
        do {
            _ = try await scopedClient().createIdentity()
        } catch let error as APIError {
            guard current(authorization) else { return }
            loadAuthorization = authorization
            cachedLoad = .failed(error.errorDescription ?? "Could not create an inbox.")
            return
        } catch is AuthError {
            guard current(authorization) else { return }
            loadAuthorization = authorization
            cachedLoad = .failed("Your session expired. Sign in again.")
            return
        } catch {
            guard current(authorization) else { return }
            loadAuthorization = authorization
            cachedLoad = .failed("Could not create an inbox.")
            return
        }
        guard current(authorization) else { return }
        await loadIdentities()
    }

    /// A mailbox on this account whose handle sits inside `namespace`, if there is one.
    func mailbox(inNamespace namespace: String) -> Mailbox? {
        mailboxes.first { $0.handle?.hasPrefix(namespace + "/") ?? false }
    }

    /// Make sure a namespace this account owns has somewhere to receive mail.
    ///
    /// Buying a name grants the right to mint under it, which is not the same as having an address:
    /// until a mailbox carries the name, `/alp` resolves to nothing and appears in no list — so the
    /// screen that has just said the name is yours would go on showing no sign of it. The postbox
    /// mints one at the moment of purchase now, and this covers the two cases that cannot reach:
    /// a namespace bought before that existed, and one granted by some route other than the store.
    ///
    /// Quiet on failure. The name is bought either way, and a refusal here is not something to put
    /// in front of somebody who has just paid — `mailbox(inNamespace:)` simply keeps saying no, and
    /// the next visit to Settings tries again.
    @discardableResult
    func ensureMailbox(inNamespace namespace: String) async -> Mailbox? {
        let authorization = session.authorizationId
        guard current(authorization) else { return nil }
        let client = scopedClient()
        if let existing = mailbox(inNamespace: namespace) { return existing }
        // The claim normally created /name/main already. Reload before attempting another mint.
        await loadIdentities()
        guard current(authorization) else { return nil }
        if let existing = mailbox(inNamespace: namespace) { return existing }
        do {
            _ = try await client.createIdentity(handle: "\(namespace)/main")
        } catch {
            // Another device may have created it between the read and the mint.
            guard current(authorization) else { return nil }
            await loadIdentities()
            guard current(authorization) else { return nil }
            return mailbox(inNamespace: namespace)
        }
        guard current(authorization) else { return nil }
        await loadIdentities()
        guard current(authorization) else { return nil }
        return mailbox(inNamespace: namespace)
    }

    #if DEBUG
    func installFixtures(mailboxes: [Mailbox], me: Mailbox) {
        cachedMailboxes = mailboxes
        cachedMe = me
        cachedAuthorization = session.authorizationId
        cachedSubject = session.subject
        cachedLoad = .ready
        features.installFixture()
    }

    /// A load that finished and found nothing — the state of every account on its first sign-in.
    func installEmptyFixture() {
        cachedMailboxes = []
        cachedMe = nil
        cachedAuthorization = session.authorizationId
        cachedSubject = session.subject
        cachedLoad = .ready
    }
    #endif

    func act(as mailbox: Mailbox) {
        guard let owned = mailboxes.first(where: { $0.address == mailbox.address }) else { return }
        if cachedMe?.address != owned.address { retireActor() }
        cachedMe = owned
        defaults.set(owned.address, forKey: rememberedKey)
    }

    /// The account's other mailboxes. On a namespace these are the sub-agents, and they are the
    /// people the owner most wants to write to.
    ///
    /// No entitlement check is wanted here. An account holds the mailboxes it holds: a free account
    /// has one anonymous mailbox and this list comes out empty on its own, while a namespace owner
    /// sees their fleet. Asking the app to decide who is paid would only add a second, weaker
    /// answer to a question the server has already settled.
    var ownAgents: [Mailbox] {
        mailboxes.filter { $0.address != me?.address }
    }

    /// Which mailbox opens by default.
    ///
    /// An explicit earlier choice wins over everything. Otherwise: a handle is a mailbox somebody
    /// deliberately named — usually the one they think of as "my inbox" — while an anonymous `/k/`
    /// address is typically an agent's. Within the operator's own namespace, `main` is the one that
    /// answers for the namespace itself, so mail sent to `/bekir` rather than to `/bekir/agent1`
    /// lands there; opening any other mailbox by default would hide exactly the mail a person sent
    /// them directly.
    private func preferred(among all: [Mailbox]) -> Mailbox? {
        let remembered = defaults.string(forKey: rememberedKey)
        if let remembered, let match = all.first(where: { $0.address == remembered }) { return match }

        let named = all.filter { $0.handle != nil }
        let inNamespace = { (mailbox: Mailbox) in
            mailbox.handle?.hasPrefix(Config.primaryNamespace + "/") ?? false
        }
        return named.first { $0.handle == Config.primaryNamespace + "/main" }
            ?? named.first(where: inNamespace)
            // A /github/<login> mailbox is personal by construction: the postbox mints one only for
            // a login the account has proved it controls.
            ?? named.first { $0.handle?.hasPrefix("/github/") ?? false }
            ?? named.first { $0.handle?.hasSuffix("/main") ?? false }
            ?? named.first
            ?? all.first
    }

    /// Set by the app so signing out also stops this device being woken. A back-reference rather
    /// than a dependency in the initializer: push needs the account to register, and the account
    /// needs push only for this one moment.
    weak var push: PushService?
    /// Likewise for groups: signing out revokes this device's group keys and erases them here.
    weak var groups: GroupStore?

    /// Somebody chose to sign out. Ends the session at the realm too — see `Session.signOut`.
    func signOut() async {
        let authorization = session.authorizationId
        guard current(authorization) else { return }
        // Before the token goes: unregistering needs a live session, and a phone that keeps ringing
        // for mail it can no longer open is worse than one that goes quiet.
        if let push {
            await push.unregister()
        }
        guard session.authorizationId == authorization else { return }
        retiringAuthorization = authorization
        retireActor()
        await groups?.signOut(expectedAuthorization: authorization)
        guard session.authorizationId == authorization else { return }
        forgetLocal()
        await session.signOut(expectedAuthorization: authorization)
    }

    /// The realm stopped accepting this session on its own. Nothing to end and nobody to tell:
    /// unregistering push would need the very token that was just refused, and a logout call would
    /// be a round trip to be told what we already know.
    func sessionExpired() {
        forgetLocal()
        session.forget()
    }

    private func forgetLocal() {
        purchaseStore?.reset()
        purchaseStore = nil
        features.reset()
        cachedMailboxes = []
        cachedMe = nil
        cachedAuthorization = nil
        cachedSubject = nil
        cachedLoad = .idle
        loadRequest = nil
        loadAuthorization = nil
        creationRequest = nil
        creatingAuthorization = nil
        retiringAuthorization = nil
        retireActor()
        defaults.removeObject(forKey: rememberedKey)
    }
}
