import CryptoKit
import Foundation

enum StrongSignInProblem: LocalizedError {
    case unverified, differentAccount
    var errorDescription: String? {
        switch self {
        case .unverified: return "The fresh sign-in could not be verified. Try again."
        case .differentAccount: return "Use the account already signed in to Pigeonpost."
        }
    }
}

/// Applies only to the response of our direct, fixed HTTPS token exchange (OIDC Core 3.1.3.7).
/// This is not an API for trusting uploaded tokens; the postbox independently verifies its bearer.
enum StrongSignInProof {
    private struct Header: Decodable { let alg: String }
    private struct Claims: Decodable {
        let iss: String
        let sub: String
        let aud: [String]
        let azp: String?
        let exp: Double
        let iat: Double
        let nonce: String
        let acr: String
        let authTime: Double
        let accessHash: String?
        enum CodingKeys: String, CodingKey {
            case iss, sub, aud, azp, exp, iat, nonce, acr
            case authTime = "auth_time", accessHash = "at_hash"
        }
        init(from decoder: Decoder) throws {
            let values = try decoder.container(keyedBy: CodingKeys.self)
            iss = try values.decode(String.self, forKey: .iss); sub = try values.decode(String.self, forKey: .sub)
            if let list = try? values.decode([String].self, forKey: .aud) { aud = list }
            else { aud = [try values.decode(String.self, forKey: .aud)] }
            azp = try values.decodeIfPresent(String.self, forKey: .azp)
            exp = try values.decode(Double.self, forKey: .exp); iat = try values.decode(Double.self, forKey: .iat)
            nonce = try values.decode(String.self, forKey: .nonce); acr = try values.decode(String.self, forKey: .acr)
            authTime = try values.decode(Double.self, forKey: .authTime)
            accessHash = try values.decodeIfPresent(String.self, forKey: .accessHash)
        }
    }

    static func validate(idToken: String?, accessToken: String, subject: String, nonce: String,
                         requirement: StrongSignInRequirement, startedAt: Date, now: Date = Date()) throws {
        guard !subject.isEmpty, subject.utf8.count <= 256,
              let idToken, let id = parts(idToken), let access = parts(accessToken),
              let header = try? JSONDecoder().decode(Header.self, from: id[0]), header.alg == "RS256",
              let claims = try? JSONDecoder().decode(Claims.self, from: id[1]),
              let accessClaims = (try? JSONSerialization.jsonObject(with: access[1])) as? [String: Any]
        else { throw StrongSignInProblem.unverified }
        guard claims.sub == subject, accessClaims["sub"] as? String == subject else { throw StrongSignInProblem.differentAccount }
        let instant = now.timeIntervalSince1970, began = startedAt.timeIntervalSince1970
        guard claims.iss == Config.OIDC.issuer.absoluteString,
              accessClaims["iss"] as? String == Config.OIDC.issuer.absoluteString,
              claims.aud == [Config.OIDC.clientId], claims.azp == nil || claims.azp == Config.OIDC.clientId,
              !nonce.isEmpty, nonce.utf8.count <= 128, claims.nonce == nonce,
              claims.exp.isFinite, claims.exp > instant, claims.exp > claims.iat, claims.iat.isFinite,
              claims.iat >= began - 30, claims.iat <= instant + 30,
              claims.authTime.isFinite, claims.authTime <= instant + 30,
              claims.authTime >= instant - Double(requirement.maxAge) - 30,
              requirement.maxAge != 0 || claims.authTime >= began - 5,
              requirement.acrValues.contains(claims.acr)
        else { throw StrongSignInProblem.unverified }
        if let hash = claims.accessHash {
            let bytes = Data(SHA256.hash(data: Data(accessToken.utf8))).prefix(16)
            let expected = Data(bytes).base64EncodedString().replacingOccurrences(of: "+", with: "-")
                .replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
            guard hash == expected else { throw StrongSignInProblem.unverified }
        }
    }

    static func authorizationCode(from callback: URL, expectedState: String) throws -> String {
        guard callback.absoluteString.utf8.count <= 16_384,
              let parts = URLComponents(url: callback, resolvingAgainstBaseURL: false),
              let target = URLComponents(string: Config.OIDC.redirectURI),
              parts.scheme == target.scheme, parts.host == target.host,
              parts.percentEncodedPath == target.percentEncodedPath,
              parts.user == nil, parts.password == nil, parts.port == nil, parts.fragment == nil
        else { throw StrongSignInProblem.unverified }
        let items = parts.queryItems ?? []
        guard Set(items.map(\.name)).count == items.count,
              items.first(where: { $0.name == "state" })?.value == expectedState,
              items.first(where: { $0.name == "error" }) == nil,
              items.first(where: { $0.name == "iss" }).map({ $0.value == Config.OIDC.issuer.absoluteString }) ?? true,
              let code = items.first(where: { $0.name == "code" })?.value,
              !code.isEmpty, code.utf8.count <= 4096, !code.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) })
        else { throw StrongSignInProblem.unverified }
        return code
    }

    private static func parts(_ token: String) -> [Data]? {
        guard token.utf8.count <= 32_768 else { return nil }
        let pieces = token.split(separator: ".", omittingEmptySubsequences: false)
        guard pieces.count == 3, pieces.allSatisfy({ !$0.isEmpty && $0.utf8.allSatisfy {
            (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0) || $0 == 45 || $0 == 95
        } }) else { return nil }
        var output: [Data] = []
        for piece in pieces.prefix(2) {
            var base = piece.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
            base += String(repeating: "=", count: (4 - base.count % 4) % 4)
            guard let data = Data(base64Encoded: base) else { return nil }; output.append(data)
        }
        return output
    }
}

/// Credentials never follow a token/logout POST to another URL.
final class TokenEndpointRedirectPolicy: NSObject, URLSessionTaskDelegate, @unchecked Sendable {
    func urlSession(_ session: URLSession, task: URLSessionTask, willPerformHTTPRedirection response: HTTPURLResponse,
                    newRequest request: URLRequest, completionHandler: @escaping (URLRequest?) -> Void) {
        completionHandler(nil)
    }
}
