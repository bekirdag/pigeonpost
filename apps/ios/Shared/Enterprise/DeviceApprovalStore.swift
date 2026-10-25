import Foundation
import Observation

@MainActor protocol DeviceApprovalSession: TokenProviding {
    var subject: String? { get }
    var authorizationId: UUID { get }
    var deviceSignedIn: Bool { get }
    func verifyStrongSignIn(_ requirement: StrongSignInRequirement) async throws -> UUID
}

extension Session: DeviceApprovalSession {
    var deviceSignedIn: Bool { status == .signedIn }
}

@MainActor @Observable final class DeviceApprovalStore {
    enum Phase: Equatable { case idle, resolving, review, approving, denying, verifying, approved, denied, expired, unconfirmed, unavailable }
    private struct Scope: Equatable { let subject: String; let authorization: UUID }
    private weak var session: (any DeviceApprovalSession)?
    private let base: URL
    private var scope: Scope?
    private var operation = UUID()
    private(set) var proofId = UUID()
    private(set) var code: DevicePairingCode?
    private(set) var preview: DevicePairingPreview?
    private(set) var phase = Phase.idle
    private(set) var message: String?
    private(set) var challenge: StrongSignInRequirement?
    private(set) var retryAt: Date?

    init(session: any DeviceApprovalSession, base: URL = Config.postbox) {
        self.session = session; self.base = base; scope = currentScope
    }
    var busy: Bool { [.resolving, .approving, .denying, .verifying].contains(phase) }
    var contextMatches: Bool { scope != nil && scope == currentScope }
    var canApprove: Bool {
        contextMatches && phase == .review && challenge == nil && preview?.allowsApproval(at: Date()) == true
    }
    var canDeny: Bool { contextMatches && phase == .review && preview != nil }

    private var currentScope: Scope? {
        guard let session, session.deviceSignedIn, let subject = session.subject, !subject.isEmpty else { return nil }
        return Scope(subject: subject, authorization: session.authorizationId)
    }
    func synchronize() {
        let next = currentScope
        guard next != scope else { return }
        // Only the awaited, verified result may adopt a same-person authentication transition.
        if phase == .verifying, next?.subject == scope?.subject, next != nil { return }
        retire(); scope = next
    }
    func retire() {
        operation = UUID(); proofId = UUID(); code = nil; preview = nil; challenge = nil
        phase = .idle; message = nil
        retryAt = nil
    }
    func changeCode() {
        let deadline = retryAt
        retire()
        retryAt = deadline
        if let deadline, deadline > Date() { message = "Wait before checking another request." }
    }
    func refreshExpiry() {
        if let retryAt, retryAt <= Date() { self.retryAt = nil }
        guard phase == .review, let preview, preview.expiresAt <= Int(Date().timeIntervalSince1970) else { return }
        proofId = UUID(); phase = .expired; challenge = nil; message = "This request expired. Start again on the machine."
    }

    func resolve(_ code: DevicePairingCode) async {
        synchronize()
        if let retryAt, retryAt > Date() { message = "Wait before checking another request."; return }
        guard let scope, contextMatches else { message = "Sign in before reviewing a machine."; return }
        let attempt = UUID(); operation = attempt; proofId = UUID()
        self.code = code; preview = nil; challenge = nil; message = nil; phase = .resolving
        guard let (client, tokens) = client(scope: scope, attempt: attempt) else { return }
        defer { withExtendedLifetime(tokens) {} }
        do {
            let loaded = try await client.resolveDevicePairing(code)
            guard current(scope: scope, attempt: attempt) else { synchronize(); return }
            apply(loaded)
        } catch {
            guard current(scope: scope, attempt: attempt) else { synchronize(); return }
            fail(error)
        }
    }
    func checkStatus() async { if let code { await resolve(code) } }

    func approve(proof: UUID) async {
        refreshExpiry()
        guard proof == proofId, canApprove, let preview, let code, let scope else { return }
        let attempt = UUID(); operation = attempt; phase = .approving; message = nil
        guard let (client, tokens) = client(scope: scope, attempt: attempt) else { return }
        defer { withExtendedLifetime(tokens) {} }
        do {
            try await client.approveDevicePairing(preview, code: code)
            guard current(scope: scope, attempt: attempt) else { synchronize(); return }
            proofId = UUID(); phase = .approved; challenge = nil
        } catch let error as APIError where error.needsStrongSignIn {
            guard current(scope: scope, attempt: attempt) else { synchronize(); return }
            proofId = UUID(); challenge = error.signInRequirement
            phase = challenge == nil ? .unavailable : .review
            message = challenge == nil ? "The server could not provide a supported verification method." : "Verify your sign-in, then review and approve this request again."
        } catch {
            guard current(scope: scope, attempt: attempt) else { synchronize(); return }
            fail(error, uncertain: true)
        }
    }

    func deny(proof: UUID) async {
        refreshExpiry()
        guard proof == proofId, canDeny, let preview, let code, let scope else { return }
        let attempt = UUID(); operation = attempt; phase = .denying; message = nil
        guard let (client, tokens) = client(scope: scope, attempt: attempt) else { return }
        defer { withExtendedLifetime(tokens) {} }
        var acknowledged = false
        do {
            try await client.denyDevicePairing(preview, code: code)
            acknowledged = true
            let loaded = try await client.resolveDevicePairing(code)
            guard current(scope: scope, attempt: attempt) else { synchronize(); return }
            if loaded.state == "approved" || loaded.state == "denied" { apply(loaded) }
            else {
                proofId = UUID(); phase = .unconfirmed
                message = "The denial was sent, but the final status is not confirmed. Check the request again."
            }
        } catch {
            guard current(scope: scope, attempt: attempt) else { synchronize(); return }
            if acknowledged {
                proofId = UUID(); phase = .unconfirmed
                message = "The denial was sent, but the final status could not be checked."
            } else { fail(error, uncertain: true) }
        }
    }

    func verifySignIn() async {
        refreshExpiry()
        guard contextMatches, phase == .review, let challenge, let code, let scope, let session else { return }
        let attempt = UUID(); operation = attempt; proofId = UUID(); phase = .verifying; message = nil
        do {
            let accepted = try await session.verifyStrongSignIn(challenge)
            guard operation == attempt else { return }
            guard let next = currentScope, next.subject == scope.subject, next.authorization == accepted else {
                phase = .idle; synchronize(); return
            }
            self.scope = next; phase = .idle
            await resolve(code) // A read only; no approval is replayed after authentication.
        } catch {
            guard operation == attempt else { return }
            phase = .review; synchronize()
            guard contextMatches else { return }
            message = (error as? LocalizedError)?.errorDescription ?? "Sign-in was not verified. Try again."
        }
    }

    private func apply(_ loaded: DevicePairingPreview) {
        preview = loaded; proofId = UUID(); challenge = nil; message = nil
        switch loaded.state {
        case "approved": phase = .approved
        case "denied": phase = .denied
        default:
            phase = .review
            if !loaded.canApprove {
                message = loaded.reason == "delegation_required" ? "Ask an agent admin to add you as an operator." : "You cannot approve this request."
            }
            refreshExpiry()
        }
    }
    private func current(scope: Scope, attempt: UUID) -> Bool { operation == attempt && self.scope == scope && currentScope == scope }
    private func client(scope: Scope, attempt: UUID) -> (PostboxClient, DeviceApprovalTokens)? {
        guard let session else { return nil }
        let tokens = DeviceApprovalTokens(source: session, current: { [weak self] in self?.current(scope: scope, attempt: attempt) == true })
        return (PostboxClient(base: base, tokens: tokens), tokens)
    }
    private func fail(_ error: Error, uncertain: Bool = false) {
        proofId = UUID(); challenge = nil
        guard let error = error as? APIError else {
            phase = uncertain ? .unconfirmed : .idle
            message = uncertain ? "The decision is not confirmed. Check the request's status before trying again." : "Could not reach Pigeonpost. Try again."
            return
        }
        if error.status == 429 { retryAt = Date().addingTimeInterval(Double(min(3600, max(1, error.retryAfter ?? 30)))) }
        switch error.code {
        case "step_up_unavailable": phase = .unavailable; message = "Strong sign-in is not available on this server."
        case "pairing_expired": phase = .expired; message = "This request expired. Start again on the machine."
        case "pairing_not_found": phase = .idle; message = "No request found for this code. Check it on the machine."
        case "pairing_decided": phase = .unconfirmed; message = "This request was already decided. Check its current status."
        case "delegation_required", "forbidden", "human_required": phase = .unavailable; message = "Your account cannot decide this request."
        default:
            phase = uncertain ? .unconfirmed : .idle
            message = error.status == 429 ? "Too many attempts. Wait before checking again." : uncertain ? "The decision is not confirmed. Check its status." : "Could not verify this request. Try again."
        }
    }
}

@MainActor private final class DeviceApprovalTokens: TokenProviding {
    private weak var source: (any DeviceApprovalSession)?
    private let current: () -> Bool
    init(source: any DeviceApprovalSession, current: @escaping () -> Bool) { self.source = source; self.current = current }
    func token() async throws -> String {
        guard current(), let source else { throw CancellationError() }
        let token = try await source.token()
        guard current() else { throw CancellationError() }; return token
    }
    func renew() async throws -> String {
        guard current(), let source else { throw CancellationError() }
        let token = try await source.renew()
        guard current() else { throw CancellationError() }; return token
    }
}
