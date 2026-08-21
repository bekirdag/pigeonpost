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

    private let anchor = PresentationAnchor()
    private var webSession: ASWebAuthenticationSession?

    private enum Key {
        static let access = "access_token"
        static let refresh = "refresh_token"
    }

    init() {
        // Through locals, and `status` first: @Observable turns these into computed properties over
        // its registrar, so reading one back before every stored property is initialized is
        // touching `self` too early.
        let access = Keychain.read(Key.access)
        let refresh = Keychain.read(Key.refresh)
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

    #if DEBUG
    /// Used only by `-fixtures`: no token is minted and none is stored, so nothing this sets can
    /// reach the postbox.
    func installFixtureSession() {
        status = .signedIn
    }
    #endif

    // ---- the flow ---------------------------------------------------------------------------

    func signIn() async {
        lastError = nil
        do {
            let verifier = Self.randomToken(64)
            let expectedState = Self.randomToken(16)
            let callback = try await authorize(verifier: verifier, state: expectedState)
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
            adopt(tokens)
            status = .signedIn
        } catch AuthError.cancelled {
            // Backing out of the sign-in sheet is an answer, not a fault. Saying "sign-in failed"
            // for it reads as an error the person then goes looking for.
        } catch {
            lastError = error.localizedDescription
        }
    }

    func signOut() {
        refreshTask?.cancel()
        refreshTask = nil
        accessToken = nil
        refreshToken = nil
        Keychain.delete(Key.access)
        Keychain.delete(Key.refresh)
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
        if let refreshTask { return try await refreshTask.value }
        guard let refreshToken else {
            signOut()
            throw AuthError.sessionExpired
        }
        let task = Task<String, Error> { [weak self] in
            guard let self else { throw AuthError.sessionExpired }
            let tokens = try await self.exchange([
                "grant_type": "refresh_token",
                "client_id": Config.OIDC.clientId,
                "refresh_token": refreshToken,
            ])
            guard let access = tokens.accessToken else { throw AuthError.sessionExpired }
            self.adopt(tokens)
            return access
        }
        refreshTask = task
        defer { refreshTask = nil }
        do {
            return try await task.value
        } catch let error as AuthError {
            // The realm said no. A dead refresh token is a dead session, and retrying it only
            // spends the rest of them.
            if case .sessionExpired = error { signOut() }
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

    // ---- the realm --------------------------------------------------------------------------

    private struct TokenResponse: Decodable {
        let accessToken: String?
        let refreshToken: String?
        let expiresIn: Int?
        let error: String?
        let errorDescription: String?

        enum CodingKeys: String, CodingKey {
            case accessToken = "access_token"
            case refreshToken = "refresh_token"
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

        let (data, response) = try await URLSession.shared.data(for: request)
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

    private func authorize(verifier: String, state: String) async throws -> URL {
        var components = URLComponents(url: Config.OIDC.endpoint("auth"), resolvingAgainstBaseURL: false)!
        components.queryItems = [
            URLQueryItem(name: "client_id", value: Config.OIDC.clientId),
            URLQueryItem(name: "response_type", value: "code"),
            URLQueryItem(name: "scope", value: Config.OIDC.scope),
            URLQueryItem(name: "redirect_uri", value: Config.OIDC.redirectURI),
            URLQueryItem(name: "code_challenge", value: Self.challenge(for: verifier)),
            URLQueryItem(name: "code_challenge_method", value: "S256"),
            URLQueryItem(name: "state", value: state),
        ]
        let url = components.url!

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
            // Not ephemeral: the realm's own session is the point of single sign-on, and an app that
            // demands the password again when the browser did not is an app people sign out of.
            session.prefersEphemeralWebBrowserSession = false
            webSession = session
            if !session.start() {
                continuation.resume(throwing: AuthError.cannotPresent)
            }
        }
    }

    // ---- small things ------------------------------------------------------------------------

    private static func randomToken(_ bytes: Int) -> String {
        var buffer = [UInt8](repeating: 0, count: bytes)
        _ = SecRandomCopyBytes(kSecRandomDefault, bytes, &buffer)
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
private final class PresentationAnchor: NSObject, ASWebAuthenticationPresentationContextProviding {
    func presentationAnchor(for session: ASWebAuthenticationSession) -> ASPresentationAnchor {
        MainActor.assumeIsolated {
            UIApplication.shared.connectedScenes
                .compactMap { $0 as? UIWindowScene }
                .flatMap(\.windows)
                .first { $0.isKeyWindow } ?? ASPresentationAnchor()
        }
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
