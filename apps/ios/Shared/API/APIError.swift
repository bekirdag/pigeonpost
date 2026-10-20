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

    init(status: Int, code: String?, detail: String?) {
        self.status = status
        self.code = code
        self.detail = detail
    }

    /// From a refusal's body. The postbox says `detail` on older routes and `message` on newer ones.
    init(status: Int, body: Data, retryAfter: Int? = nil) {
        let object = (try? JSONSerialization.jsonObject(with: body)) as? [String: Any]
        self.status = status
        code = object?["error"] as? String
        detail = (object?["detail"] as? String) ?? (object?["message"] as? String)
        currentEpoch = (object?["current_epoch"] as? NSNumber)?.intValue
        currentRevision = (object?["current_revision"] as? NSNumber)?.intValue
            ?? (object?["revision"] as? NSNumber)?.intValue
        self.retryAfter = retryAfter
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
