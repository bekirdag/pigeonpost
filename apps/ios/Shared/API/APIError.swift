import Foundation

struct APIError: LocalizedError, Equatable {
    let status: Int
    /// The postbox's own machine-readable code — `not_admitted`, `recipient_unresolved`, and so on.
    let code: String?
    let detail: String?

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
