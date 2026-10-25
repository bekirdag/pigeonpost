//  Sign-in, and the one place that knows what a live token is.
//
//  PKCE against the realm, exactly as the web app does it — public client, no secret, S256. What is
//  different here is where the tokens live (the Keychain, not localStorage) and when they are
//  renewed. The browser schedules a timer off the token's own `exp`; a phone cannot rely on a timer
//  it may sleep through, so every call asks `token()` and that is the only clock. A token with less
//  than a minute left is refreshed before it is used rather than after it fails.

import AuthenticationServices
import CryptoKit
import Foundation
import Observation
#if canImport(UIKit)
import UIKit
#elseif canImport(AppKit)
import AppKit
#endif

enum AuthError: LocalizedError {
    case cancelled
    case cannotPresent
    case stateMismatch
    case noCode(String?)
    case exchangeFailed(String)
    case sessionExpired

    var errorDescription: String? {
        switch self {
        case .cancelled: return "Sign-in was cancelled."
        case .cannotPresent: return "Could not open the sign-in page."
        case .stateMismatch: return "Sign-in could not be verified. Please try again."
        case .noCode(let error): return error.map { "Sign-in did not complete: \($0)" } ?? "Sign-in did not complete."
        case .exchangeFailed(let detail): return detail
        case .sessionExpired: return "Your session expired. Sign in again."
        }
    }
}

@MainActor
@Observable
final class Session {
    enum Status: Equatable { case signedOut, signedIn }

    private(set) var status: Status
    /// Changes on a new authorization or local retirement, including a return to the same person.
    private(set) var authorizationId = UUID()
    /// Set when a sign-in attempt failed, so the signed-out screen can say why rather than simply
    /// returning to itself — a misconfigured redirect URI is otherwise indistinguishable from a
    /// wrong password.
    var lastError: String?

    private var accessToken: String?
    private var refreshToken: String?

    /// One in-flight refresh, shared: a burst of 401s must not spend a rotating refresh token
    /// twice. The realm rotates them, so the second spender loses and the session dies for no
    /// reason.
    private var refreshTask: Task<String, Error>?
    private var refreshTaskId: UUID?
    private var authorizationAttempt: UUID?

    private let anchor = PresentationAnchor()
    private var webSession: ASWebAuthenticationSession?
    private let network: URLSession
    private let browserAuthorization: (@MainActor (URL) async throws -> URL)?

    private enum Key {
        static let access = "access_token"
        static let refresh = "refresh_token"
    }

    init(network: URLSession = .shared, browserAuthorization: (@MainActor (URL) async throws -> URL)? = nil) {
        self.network = network
        self.browserAuthorization = browserAuthorization
        // Through locals, and `status` first: @Observable turns these into computed properties over
        // its registrar, so reading one back before every stored property is initialized is
        // touching `self` too early.
        // A fixture launch is a demo mailbox and must not pick up, or later act on, a real session.
        let access = Fixtures.enabled ? nil : Keychain.read(Key.access)
        let refresh = Fixtures.enabled ? nil : Keychain.read(Key.refresh)
        // A stored refresh token is a session even when the access token beside it is spent: the
        // first call will renew it. Only the absence of both is signed out.
        status = (refresh != nil || access != nil) ? .signedIn : .signedOut
        accessToken = access
        refreshToken = refresh
    }

    /// Who the realm says is signed in. Read from the access token's own claims rather than from a
    /// userinfo call — it is display text, and one round-trip for a name is one too many.
    ///
    /// `preferred_username` only. The realm always sends it, and the other claim that would do is
    /// one this repository does not use the name of.
    var username: String? {
        guard let accessToken, let claims = Self.claims(of: accessToken) else { return nil }
        return claims["preferred_username"] as? String
    }

    /// Local operation/pending-purchase scope. Entitlement authorization remains on the postbox.
    var subject: String? {
        guard let accessToken, let claims = Self.claims(of: accessToken) else { return nil }
        return claims["sub"] as? String
    }

    #if DEBUG
    /// Used only by `-fixtures`: no token is minted and none is stored, so nothing this sets can
    /// reach the postbox.
    func installFixtureSession() {
        status = .signedIn
    }
    #endif

    // ---- the flow ---------------------------------------------------------------------------

    /// `otherAccount` is the deliberate "not this one" path.
    ///
    /// Ending the realm session is not enough on its own: sign-in through an identity provider
    /// leaves that provider's own cookie in the browser, and it will happily answer for the account
    /// already signed in there — so the person is returned to the identity they were trying to
    /// leave, having been shown no choice. `prompt=login` asks the realm not to reuse a session, and
    /// an ephemeral browser guarantees it by carrying no cookie any provider could recognise.
    func signIn(otherAccount: Bool = false, provider: Config.OIDC.IdentityProvider? = nil) async {
        let context = beginAuthorization()
        defer {
            if authorizationAttempt == context.attempt { authorizationAttempt = nil; webSession = nil }
        }
        lastError = nil
        if provider == .apple, !otherAccount, await signInWithAppleNatively(context: context) { return }
        guard isCurrent(context) else { return }
        do {
            let verifier = try Self.randomToken(64)
            let expectedState = try Self.randomToken(16)
            let callback = try await authorize(verifier: verifier, state: expectedState,
                                               otherAccount: otherAccount, provider: provider)
            guard isCurrent(context) else { return }
            let items = URLComponents(url: callback, resolvingAgainstBaseURL: false)?.queryItems ?? []
            let value = { (name: String) in items.first { $0.name == name }?.value }

            guard value("state") == expectedState else { throw AuthError.stateMismatch }
            guard let code = value("code") else { throw AuthError.noCode(value("error")) }

            let tokens = try await exchange([
                "grant_type": "authorization_code",
                "client_id": Config.OIDC.clientId,
                "code": code,
                "redirect_uri": Config.OIDC.redirectURI,
                "code_verifier": verifier,
            ])
            accept(tokens, for: context)
        } catch AuthError.cancelled {
            // Backing out of the sign-in sheet is an answer, not a fault. Saying "sign-in failed"
            // for it reads as an error the person then goes looking for.
        } catch {
            if isCurrent(context) { lastError = error.localizedDescription }
        }
    }

    /// Refresh authentication for this exact person; the caller must obtain another approval.
    @discardableResult
    func verifyStrongSignIn(_ requirement: StrongSignInRequirement) async throws -> UUID {
        guard status == .signedIn else { throw AuthError.sessionExpired }
        let original = authorizationId
        _ = try await token()
        guard authorizationId == original, status == .signedIn, let subject, !subject.isEmpty else { throw CancellationError() }
        let context = beginAuthorization()
        defer {
            if authorizationAttempt == context.attempt { authorizationAttempt = nil; webSession = nil }
        }
        let startedAt = Date()
        let verifier = try Self.randomToken(64), state = try Self.randomToken(16), nonce = try Self.randomToken(32)
        let callback = try await authorize(verifier: verifier, state: state, otherAccount: false, provider: nil,
                                           nonce: nonce, requirement: requirement)
        guard isCurrent(context) else { throw CancellationError() }
        let code = try StrongSignInProof.authorizationCode(from: callback, expectedState: state)
        let tokens = try await exchange(["grant_type": "authorization_code", "client_id": Config.OIDC.clientId,
            "code": code, "redirect_uri": Config.OIDC.redirectURI, "code_verifier": verifier])
        guard isCurrent(context), let access = tokens.accessToken else { throw CancellationError() }
        try StrongSignInProof.validate(idToken: tokens.idToken, accessToken: access, subject: subject, nonce: nonce,
                                       requirement: requirement, startedAt: startedAt)
        accept(tokens, for: context)
        return authorizationId
    }

    /// Sign in with Apple through the system sheet, then trade Apple's identity token for realm
    /// tokens (token exchange on `pigeonpost-mobile`). The realm stays the issuer, so the account —
    /// and every token the postbox sees — is the same one the web flow reaches.
    ///
    /// Returns false when the web flow should take over instead: chiefly an existing account with
    /// the same contact address that has not been linked to Apple yet. Linking needs proof the person owns that
    /// account, and only the realm's own pages can ask for it.
    private func signInWithAppleNatively(context: AuthorizationContext) async -> Bool {
        let credential: ASAuthorizationAppleIDCredential
        do {
            credential = try await appleAuthorization.perform()
        } catch let error as ASAuthorizationError where error.code == .canceled {
            return true
        } catch {
            // No Apple ID on the device, or the system sheet could not run: the web flow still can.
            return !isCurrent(context)
        }
        guard isCurrent(context) else { return true }
        guard let tokenData = credential.identityToken, let identityToken = String(data: tokenData, encoding: .utf8) else {
            return false
        }
        var form = [
            "grant_type": "urn:ietf:params:oauth:grant-type:token-exchange",
            "client_id": Config.OIDC.clientId,
            "subject_issuer": "apple",
            "subject_token_type": "urn:ietf:params:oauth:token-type:id_token",
            "subject_token": identityToken,
            "scope": Config.OIDC.scope,
        ]
        // Apple tells the app the person's name only the first time; the token never carries it.
        if let name = credential.fullName, name.givenName != nil || name.familyName != nil {
            let profile: [String: Any] = ["name": ["firstName": name.givenName ?? "", "lastName": name.familyName ?? ""]]
            if let json = try? JSONSerialization.data(withJSONObject: profile) {
                form["user_profile"] = String(data: json, encoding: .utf8)
            }
        }
        do {
            let tokens = try await exchange(form)
            accept(tokens, for: context)
            return true
        } catch {
            return !isCurrent(context)
        }
    }

    private let appleAuthorization = AppleAuthorization()

    /// Retire this authorization locally, then end its session at the realm.
    ///
    /// Deleting the tokens alone is not signing out. The realm's own SSO session outlives them, and
    /// the browser still holds its cookie — so the next sign-in was answered silently, without ever
    /// showing a login screen, and there was no way to choose a different account. Ending the realm
    /// session is what makes that cookie worthless.
    ///
    /// Back-channel on purpose. The browser-based logout would do this too, but it opens a web sheet
    /// (and, the first time, an "…wants to sign you in" alert) in the middle of signing *out*, which
    /// reads as a bug. A POST carrying the refresh token identifies the session just as well.
    func signOut() async {
        let refresh = refreshToken
        forget()
        if let refresh {
            var request = URLRequest(url: Config.OIDC.endpoint("logout"))
            request.httpMethod = "POST"
            request.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "content-type")
            request.httpBody = Data(Self.formEncode([
                "client_id": Config.OIDC.clientId,
                "refresh_token": refresh,
            ]).utf8)
            // Best effort, and deliberately not surfaced. A failure here means the realm session
            // outlives this device — worth fixing, never worth trapping somebody inside the app.
            _ = try? await network.data(for: request, delegate: TokenEndpointRedirectPolicy())
        }
    }

    /// Drop what this device holds, without talking to anybody.
    ///
    /// This is the expiry path, not the sign-out path: when the realm has already refused a refresh
    /// token, the session is over and there is nothing left to end. Opening a network call — or
    /// worse, a browser — at that moment would interrupt whatever the person was doing.
    func forget() {
        authorizationId = UUID()
        authorizationAttempt = nil
        webSession?.cancel()
        webSession = nil
        appleAuthorization.cancel()
        refreshTask?.cancel()
        refreshTask = nil
        refreshTaskId = nil
        accessToken = nil
        refreshToken = nil
        // A fixture launch never read these, so it has no business removing them either — on a
        // Mac the demo window shares the Keychain with the real app.
        if !Fixtures.enabled {
            Keychain.delete(Key.access)
            Keychain.delete(Key.refresh)
        }
        status = .signedOut
    }

    // ---- tokens -----------------------------------------------------------------------------

    /// A token good for the next minute at least. Every request goes through here.
    func token() async throws -> String {
        if let accessToken, Self.secondsLeft(accessToken) > 60 { return accessToken }
        return try await renew()
    }

    /// Spend the refresh token now, whatever the access token claims about itself. Called when the
    /// postbox has answered 401 — the token was live by its own `exp` and the realm disagreed.
    @discardableResult
    func renew() async throws -> String {
        let generation = authorizationId
        if let refreshTask {
            let access = try await refreshTask.value
            guard authorizationId == generation, status == .signedIn else { throw CancellationError() }
            return access
        }
        guard let refreshToken else {
            forget()
            throw AuthError.sessionExpired
        }
        let expectedSubject = subject
        let taskId = UUID()
        let task = Task<String, Error> { [weak self] in
            guard let self else { throw AuthError.sessionExpired }
            guard self.authorizationId == generation, !Task.isCancelled else { throw CancellationError() }
            let tokens = try await self.exchange([
                "grant_type": "refresh_token",
                "client_id": Config.OIDC.clientId,
                "refresh_token": refreshToken,
            ])
            guard self.authorizationId == generation, self.status == .signedIn, !Task.isCancelled else { throw CancellationError() }
            guard let access = tokens.accessToken else { throw AuthError.sessionExpired }
            if let expectedSubject, Self.claims(of: access)?["sub"] as? String != expectedSubject {
                throw AuthError.sessionExpired
            }
            self.adopt(tokens)
            return access
        }
        refreshTask = task
        refreshTaskId = taskId
        defer {
            if refreshTaskId == taskId { refreshTask = nil; refreshTaskId = nil }
        }
        do {
            let access = try await task.value
            guard authorizationId == generation, status == .signedIn else { throw CancellationError() }
            return access
        } catch let error as AuthError {
            // The realm said no. A dead refresh token is a dead session, and retrying it only
            // spends the rest of them.
            if case .sessionExpired = error, authorizationId == generation { forget() }
            throw error
        } catch {
            // A network blip is not a dead session: keep what we hold and let the caller retry.
            throw error
        }
    }

    private func adopt(_ tokens: TokenResponse) {
        if let access = tokens.accessToken {
            accessToken = access
            Keychain.write(Key.access, access)
        }
        // The realm rotates refresh tokens; a response without one means keep the current.
        if let refresh = tokens.refreshToken {
            refreshToken = refresh
            Keychain.write(Key.refresh, refresh)
        }
    }

    private struct AuthorizationContext {
        let attempt: UUID
        let generation: UUID
    }

    private func beginAuthorization() -> AuthorizationContext {
        let attempt = UUID()
        authorizationAttempt = attempt
        webSession?.cancel()
        webSession = nil
        appleAuthorization.cancel()
        return AuthorizationContext(attempt: attempt, generation: authorizationId)
    }

    private func isCurrent(_ context: AuthorizationContext) -> Bool {
        authorizationAttempt == context.attempt && authorizationId == context.generation
    }

    private func accept(_ tokens: TokenResponse, for context: AuthorizationContext) {
        guard isCurrent(context) else { return }
        refreshTask?.cancel()
        refreshTask = nil
        refreshTaskId = nil
        authorizationId = UUID()
        adopt(tokens)
        status = .signedIn
    }

    // ---- the realm --------------------------------------------------------------------------

    private struct TokenResponse: Decodable {
        let accessToken: String?
        let refreshToken: String?
        let idToken: String?
        let expiresIn: Int?
        let error: String?
        let errorDescription: String?

        enum CodingKeys: String, CodingKey {
            case accessToken = "access_token"
            case refreshToken = "refresh_token"
            case idToken = "id_token"
            case expiresIn = "expires_in"
            case error
            case errorDescription = "error_description"
        }
    }

    private func exchange(_ form: [String: String]) async throws -> TokenResponse {
        var request = URLRequest(url: Config.OIDC.endpoint("token"))
        request.httpMethod = "POST"
        request.setValue("application/x-www-form-urlencoded", forHTTPHeaderField: "content-type")
        request.httpBody = Data(Self.formEncode(form).utf8)

        let (data, response) = try await network.data(for: request, delegate: TokenEndpointRedirectPolicy())
        guard response.url == request.url else { throw AuthError.stateMismatch }
        let body = try? JSONDecoder().decode(TokenResponse.self, from: data)
        let status = (response as? HTTPURLResponse)?.statusCode ?? 0

        guard status == 200, let body, body.accessToken != nil else {
            // `invalid_grant` is the realm saying this credential is spent — a different thing from
            // the network being unreachable, and the only one worth signing out over.
            if body?.error == "invalid_grant" { throw AuthError.sessionExpired }
            throw AuthError.exchangeFailed(body?.errorDescription ?? body?.error ?? "The sign-in service refused the request.")
        }
        return body
    }

    private func authorize(verifier: String, state: String, otherAccount: Bool,
                           provider: Config.OIDC.IdentityProvider?, nonce: String? = nil,
                           requirement: StrongSignInRequirement? = nil) async throws -> URL {
        let url = Config.OIDC.authorizationURL(challenge: Self.challenge(for: verifier), state: state,
                                               otherAccount: otherAccount, provider: provider, nonce: nonce,
                                               acrValues: requirement?.requestedACR, maxAge: requirement?.maxAge,
                                               forceLogin: requirement != nil)
        if let browserAuthorization { return try await browserAuthorization(url) }

        return try await withCheckedThrowingContinuation { continuation in
            let session = ASWebAuthenticationSession(
                url: url,
                callbackURLScheme: Config.OIDC.redirectScheme
            ) { callback, error in
                if let callback {
                    continuation.resume(returning: callback)
                } else if let error = error as? ASWebAuthenticationSessionError,
                          error.code == .canceledLogin {
                    continuation.resume(throwing: AuthError.cancelled)
                } else {
                    continuation.resume(throwing: error ?? AuthError.cancelled)
                }
            }
            session.presentationContextProvider = anchor
            // Ordinarily not ephemeral: the realm's own session is the point of single sign-on, and
            // an app that demands the password again when the browser did not is an app people sign
            // out of. Choosing a different account is the one case where that is exactly backwards —
            // there, a cookie answering on the person's behalf is the whole problem.
            session.prefersEphemeralWebBrowserSession = otherAccount
            webSession = session
            if !session.start() {
                continuation.resume(throwing: AuthError.cannotPresent)
            }
        }
    }

    // ---- small things ------------------------------------------------------------------------

    private static func randomToken(_ bytes: Int) throws -> String {
        var buffer = [UInt8](repeating: 0, count: bytes)
        guard SecRandomCopyBytes(kSecRandomDefault, bytes, &buffer) == errSecSuccess else { throw AuthError.stateMismatch }
        return Data(buffer).base64URLEncoded
    }

    private static func challenge(for verifier: String) -> String {
        Data(SHA256.hash(data: Data(verifier.utf8))).base64URLEncoded
    }

    private static func formEncode(_ form: [String: String]) -> String {
        var allowed = CharacterSet.alphanumerics
        allowed.insert(charactersIn: "-._~")
        return form.map { key, value in
            let encoded = value.addingPercentEncoding(withAllowedCharacters: allowed) ?? value
            return "\(key)=\(encoded)"
        }.joined(separator: "&")
    }

    /// Seconds left on a token, read from its own `exp`.
    ///
    /// Not a security check: the postbox validates every token itself. This only decides when to ask
    /// for the next one, so an unreadable token means "renew now".
    static func secondsLeft(_ token: String) -> Int {
        guard let claims = claims(of: token), let exp = claims["exp"] as? Double else { return 0 }
        return Int(exp - Date().timeIntervalSince1970)
    }

    private static func claims(of token: String) -> [String: Any]? {
        let parts = token.split(separator: ".")
        guard parts.count >= 2, let data = Data(base64URLEncoded: String(parts[1])) else { return nil }
        return (try? JSONSerialization.jsonObject(with: data)) as? [String: Any]
    }
}

/// The window the sign-in sheet hangs from.
///
/// `ASPresentationAnchor` is a `UIWindow` on one platform and an `NSWindow` on the other, and
/// finding the key window is the only part that differs. Everything else about the sign-in — PKCE,
/// the exchange, where the tokens are kept — is the same code on both.
private final class PresentationAnchor: NSObject, ASWebAuthenticationPresentationContextProviding {
    func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        MainActor.assumeIsolated {
            #if canImport(UIKit)
            return UIApplication.shared.connectedScenes
                .compactMap { $0 as? UIWindowScene }
                .flatMap(\.windows)
                .first { $0.isKeyWindow } ?? ASPresentationAnchor()
            #elseif canImport(AppKit)
            return NSApplication.shared.keyWindow
                ?? NSApplication.shared.windows.first
                ?? ASPresentationAnchor()
            #else
            return ASPresentationAnchor()
            #endif
        }
    }
}

/// One Sign in with Apple request through the system sheet, as an async call — on the Mac, the sheet
/// attached to the app's window.
@MainActor
final class AppleAuthorization: NSObject, ASAuthorizationControllerDelegate,
                                        ASAuthorizationControllerPresentationContextProviding {
    private var continuation: CheckedContinuation<ASAuthorizationAppleIDCredential, Error>?
    private var controller: ASAuthorizationController?
    private let performRequests: @MainActor (ASAuthorizationController) -> Void

    init(performRequests: @escaping @MainActor (ASAuthorizationController) -> Void = { $0.performRequests() }) {
        self.performRequests = performRequests
        super.init()
    }

    func cancel() {
        let previous = controller
        let waiting = continuation
        controller = nil; continuation = nil
        previous?.cancel()
        waiting?.resume(throwing: ASAuthorizationError(.canceled))
    }

    func perform() async throws -> ASAuthorizationAppleIDCredential {
        cancel()
        let request = ASAuthorizationAppleIDProvider().createRequest()
        request.requestedScopes = [.fullName, .email]
        return try await withCheckedThrowingContinuation { continuation in
            self.continuation = continuation
            let controller = ASAuthorizationController(authorizationRequests: [request])
            controller.delegate = self
            controller.presentationContextProvider = self
            self.controller = controller
            performRequests(controller)
        }
    }

    nonisolated func authorizationController(controller: ASAuthorizationController,
                                             didCompleteWithAuthorization authorization: ASAuthorization) {
        MainActor.assumeIsolated {
            guard self.controller === controller else { return }
            if let credential = authorization.credential as? ASAuthorizationAppleIDCredential {
                finish(.success(credential))
            } else {
                finish(.failure(ASAuthorizationError(.unknown)))
            }
        }
    }

    nonisolated func authorizationController(controller: ASAuthorizationController, didCompleteWithError error: Error) {
        MainActor.assumeIsolated {
            guard self.controller === controller else { return }
            finish(.failure(error))
        }
    }

    nonisolated func presentationAnchor(for controller: ASAuthorizationController) -> ASPresentationAnchor {
        MainActor.assumeIsolated {
            #if canImport(UIKit)
            return UIApplication.shared.connectedScenes
                .compactMap { $0 as? UIWindowScene }
                .flatMap(\.windows)
                .first { $0.isKeyWindow } ?? ASPresentationAnchor()
            #else
            return NSApplication.shared.keyWindow
                ?? NSApplication.shared.mainWindow
                ?? NSApplication.shared.windows.first { $0.isVisible }
                ?? ASPresentationAnchor()
            #endif
        }
    }

    private func finish(_ result: Result<ASAuthorizationAppleIDCredential, Error>) {
        controller = nil
        let continuation = self.continuation
        self.continuation = nil
        continuation?.resume(with: result)
    }
}

extension Data {
    var base64URLEncoded: String {
        base64EncodedString()
            .replacingOccurrences(of: "+", with: "-")
            .replacingOccurrences(of: "/", with: "_")
            .replacingOccurrences(of: "=", with: "")
    }

    init?(base64URLEncoded input: String) {
        var value = input.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        while value.count % 4 != 0 { value.append("=") }
        self.init(base64Encoded: value)
    }
}
