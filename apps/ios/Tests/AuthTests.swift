import Foundation

@main
struct AuthTests {
    static func main() {
        let state = "state+with/special&characters=?"
        let challenge = "pkce_challenge-123"
        for provider: Config.OIDC.IdentityProvider? in [nil, .apple] {
            for otherAccount in [false, true] {
                let url = Config.OIDC.authorizationURL(challenge: challenge, state: state,
                                                       otherAccount: otherAccount, provider: provider)
                let parts = URLComponents(url: url, resolvingAgainstBaseURL: false)!
                let query = Dictionary(uniqueKeysWithValues: parts.queryItems!.map { ($0.name, $0.value!) })
                precondition(url.scheme == "https" && url.host == "auth.pigeonpost.dev")
                precondition(url.path == "/realms/pigeonpost-prod/protocol/openid-connect/auth")
                precondition(query["client_id"] == "pigeonpost-mobile")
                precondition(query["redirect_uri"] == "dev.pigeonpost.inbox://oauth2redirect")
                precondition(query["response_type"] == "code")
                precondition(query["code_challenge_method"] == "S256")
                precondition(query["code_challenge"] == challenge && query["state"] == state)
                precondition(query["scope"] == "openid profile offline_access")
                precondition(query["kc_idp_hint"] == provider?.rawValue)
                precondition(query["prompt"] == (otherAccount || provider != nil ? "login" : nil))
                precondition(query["client_secret"] == nil)
            }
        }
        print("Authentication URL tests passed (default, Apple, account switching, PKCE and state encoding).")
    }
}
