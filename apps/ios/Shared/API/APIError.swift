import Foundation

struct APIError: LocalizedError, Equatable {
    let status: Int
    /// The postbox's own machine-readable code — `not_admitted`, `recipient_unresolved`, and so on.
    let code: String?
    let detail: String?
    /// Fields some refusals carry beside the code: `current_epoch` and `current_revision` on a group
    /// conflict, `Retry-After` on a rate limit.
    var currentEpoch: Int? = nil
    var currentRevision: Int? = nil
    var retryAfter: Int? = nil
    var signInRequirement: StrongSignInRequirement? = nil
    private var headerRequiresStrongSignIn = false
    var needsStrongSignIn: Bool { status == 401 && (code == "step_up_required" || headerRequiresStrongSignIn) }

    init(status: Int, code: String?, detail: String?) {
        self.status = status
        self.code = code
        self.detail = detail
    }

    /// From a refusal's body. The postbox says `detail` on older routes and `message` on newer ones.
    init(status: Int, body: Data, retryAfter: Int? = nil, authenticationChallenge: String? = nil) {
        let object = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any]
        self.status = status
        code = object?["error"] as? String
        detail = (object?["detail"] as? String) ?? (object?["message"] as? String)
        currentEpoch = (object?["current_epoch"] as? NSNumber)?.intValue
        currentRevision = (object?["current_revision"] as? NSNumber)?.intValue
            ?? (object?["revision"] as? NSNumber)?.intValue
        self.retryAfter = retryAfter
        // A malformed/duplicate challenge is still a reason to stop an automatic replay.
        headerRequiresStrongSignIn = authenticationChallenge?.lowercased().contains("insufficient_user_authentication") == true
        if needsStrongSignIn { signInRequirement = StrongSignInRequirement(body: body, header: authenticationChallenge) }
    }

    var errorDescription: String? { detail ?? code ?? "The postbox answered \(status)." }

    /// What to say to a person when a send does not go through. The codes are the postbox's; the
    /// sentences are the web app's, so both clients fail with the same words.
    var sendFailureMessage: String {
        switch code {
        case "not_admitted": return "They are not accepting mail from this mailbox."
        case "recipient_unresolved": return "No mailbox at that address."
        case "recipient_inbox_full": return "Their inbox is full."
        case "unauthorized": return "Your session expired. Sign in again."
        default: return errorDescription ?? "Could not send."
        }
    }
}

/// An RFC 9470 challenge changes authentication requirements, never the trusted realm or redirect.
struct StrongSignInRequirement: Equatable {
    let acrValues: [String]
    let maxAge: Int
    var requestedACR: String { acrValues.joined(separator: " ") }

    init?(acrValues: [String], maxAge: Int) {
        guard !acrValues.isEmpty, acrValues.count <= 8, Set(acrValues).count == acrValues.count,
              (0...600).contains(maxAge), acrValues.allSatisfy({ value in
                  !value.isEmpty && value.utf8.count <= 128 && value.utf8.allSatisfy {
                      (65...90).contains($0) || (97...122).contains($0) || (48...57).contains($0)
                          || [45, 46, 47, 58, 95].contains($0)
                  }
              }) else { return nil }
        self.acrValues = acrValues; self.maxAge = maxAge
    }

    init?(body: Data, header: String?) {
        if let acr = Self.parameter("acr_values", in: header),
           let age = Self.parameter("max_age", in: header).flatMap(Int.init),
           let parsed = Self(acrValues: acr.split(separator: " ").map(String.init), maxAge: age) {
            self = parsed; return
        }
        guard let wire = try? JSONDecoder().decode(Wire.self, from: body),
              let parsed = Self(acrValues: wire.acrValues, maxAge: wire.maxAge) else { return nil }
        self = parsed
    }

    private struct Wire: Decodable {
        let acrValues: [String]
        let maxAge: Int
        enum CodingKeys: String, CodingKey { case acrValues = "acr_values", maxAge = "max_age" }
        init(from decoder: Decoder) throws {
            let values = try decoder.container(keyedBy: CodingKeys.self)
            if let list = try? values.decode([String].self, forKey: .acrValues) { acrValues = list }
            else { acrValues = try values.decode(String.self, forKey: .acrValues).split(separator: " ").map(String.init) }
            maxAge = try values.decode(Int.self, forKey: .maxAge)
        }
    }

    static func parameter(_ name: String, in header: String?) -> String? {
        guard let header, header.utf8.count <= 2048, header.lowercased().hasPrefix("bearer "),
              !header.unicodeScalars.contains(where: { CharacterSet.controlCharacters.contains($0) }) else { return nil }
        let fields = String(header.dropFirst(7))
        let pattern = "(?:^|,)\\s*" + NSRegularExpression.escapedPattern(for: name) + "\\s*=\\s*(?:\"([^\"]*)\"|([^,\\s\"]+))\\s*(?=,|$)"
        guard let expression = try? NSRegularExpression(pattern: pattern, options: .caseInsensitive) else { return nil }
        let matches = expression.matches(in: fields, range: NSRange(fields.startIndex..., in: fields))
        guard matches.count == 1 else { return nil }
        let match = matches[0]
        let range = match.range(at: match.range(at: 1).location == NSNotFound ? 2 : 1)
        guard let value = Range(range, in: fields) else { return nil }
        return String(fields[value])
    }
}
