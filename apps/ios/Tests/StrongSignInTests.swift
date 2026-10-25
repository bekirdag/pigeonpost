import CryptoKit
import Foundation

@main enum StrongSignInTests {
    static func main() throws {
        var checks = 0
        func check(_ value: Bool, _ label: String) { precondition(value, label); checks += 1 }
        func encoded(_ object: [String: Any]) throws -> String {
            try JSONSerialization.data(withJSONObject: object).base64EncodedString()
                .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
        }
        let now = Date(timeIntervalSince1970: 1_800_000_000)
        let requirement = StrongSignInRequirement(acrValues: ["mfa"], maxAge: 600)!
        let access = try encoded(["alg": "RS256"]) + "." + encoded(["sub": "member", "iss": Config.OIDC.issuer.absoluteString]) + ".fixture"
        let base: [String: Any] = ["iss": Config.OIDC.issuer.absoluteString, "sub": "member", "aud": Config.OIDC.clientId,
            "exp": 1_800_003_600, "iat": 1_800_000_000, "auth_time": 1_800_000_000, "nonce": "request-nonce", "acr": "mfa"]
        func valid(_ changes: [String: Any] = [:], algorithm: String = "RS256", request: StrongSignInRequirement? = nil) throws -> Bool {
            let id = try encoded(["alg": algorithm]) + "." + encoded(base.merging(changes) { _, new in new }) + ".fixture"
            do {
                try StrongSignInProof.validate(idToken: id, accessToken: access, subject: "member", nonce: "request-nonce", requirement: request ?? requirement, startedAt: now, now: now)
                return true
            } catch { return false }
        }
        check(try valid(), "direct response proof")
        check(try valid(["aud": [Config.OIDC.clientId], "azp": Config.OIDC.clientId]), "one trusted audience")
        for change: [String: Any] in [
            ["iss": "https://elsewhere.invalid"], ["sub": "different"], ["aud": "different-client"],
            ["aud": [Config.OIDC.clientId, "untrusted"]], ["azp": "different-client"], ["exp": 1_800_000_000],
            ["iat": 1_800_000_031], ["iat": 1_799_999_969], ["nonce": "other-request"], ["nonce": NSNull()],
            ["acr": "1"], ["auth_time": 1_799_999_369], ["auth_time": 1_800_000_031], ["auth_time": true]
        ] { check(try !valid(change), "reject substituted or stale proof") }
        check(try !valid(algorithm: "none"), "unsigned algorithm refused")
        check(try !valid(algorithm: "HS256"), "unexpected algorithm refused")
        let zero = StrongSignInRequirement(acrValues: ["mfa"], maxAge: 0)!
        check(try valid(request: zero), "fresh authentication")
        check(try !valid(["auth_time": 1_799_999_990], request: zero), "zero age requires this interaction")
        let hash = Data(Data(SHA256.hash(data: Data(access.utf8))).prefix(16)).base64EncodedString()
            .replacingOccurrences(of: "+", with: "-").replacingOccurrences(of: "/", with: "_").replacingOccurrences(of: "=", with: "")
        check(try valid(["at_hash": hash]), "optional bearer binding")
        check(try !valid(["at_hash": "wrong"]), "wrong bearer binding refused")
        let correct = Config.OIDC.redirectURI + "?code=synthetic&state=expected"
        check(try StrongSignInProof.authorizationCode(from: URL(string: correct)!, expectedState: "expected") == "synthetic", "fixed callback")
        for url in [
            "dev.pigeonpost.inbox://different?code=synthetic&state=expected", correct+"&state=other", correct+"&code=other",
            correct+"&iss=https://elsewhere.invalid", correct+"&error=access_denied", correct+"#other",
            "dev.pigeonpost.inbox://oauth2redirect?code=&state=expected", "dev.pigeonpost.inbox://oauth2redirect?code=synthetic&state=other"
        ] {
            do { _ = try StrongSignInProof.authorizationCode(from: URL(string: url)!, expectedState: "expected"); preconditionFailure("bad callback accepted") }
            catch { checks += 1 }
        }
        print("Strong sign-in: \(checks) checks passed")
    }
}
