//  What this postbox has switched on, from `GET /v1/capabilities`.
//
//  Every Phase 1b and Phase 3 feature asks here first and stays out of sight when the answer is no —
//  an app released ahead of its server must look exactly like the one before it, and an app talking
//  to an older postbox must not offer buttons that answer 404. The web inbox makes the same check
//  (`loadCapabilities` in site-inbox/app.js) and these are the same names.

import Foundation
import Observation

@MainActor
@Observable
final class Features {
    /// Phase 1b.
    static let messageFiltering = "message_filtering"
    static let callAdmission = "call_admission"
    static let callHistory = "call_history"
    static let avatars = "avatars"
    /// Phase 3a.
    static let groups = "groups"
    static let disappearing = "disappearing"
    static let messageEdit = "message_edit"
    static let messageReply = "message_reply"
    static let receipts = "receipts"
    static let agentStatus = "agent_status"
    static let calendar = "calendar"
    static let calendarEmail = "calendar_email"
    static let orgs = "orgs"

    private(set) var enabled: Set<String> = []
    /// Whether an answer has arrived at all. Nothing is offered before it has.
    private(set) var loaded = false

    func has(_ name: String) -> Bool { enabled.contains(name) }

    var callHistory: Bool { has(Self.callHistory) }
    var filtering: Bool { has(Self.messageFiltering) }
    var avatars: Bool { has(Self.avatars) }
    var groups: Bool { has(Self.groups) }
    var disappearing: Bool { has(Self.disappearing) }
    var messageEdit: Bool { has(Self.messageEdit) }
    var messageReply: Bool { has(Self.messageReply) }
    var receipts: Bool { has(Self.receipts) }
    var agentStatus: Bool { has(Self.agentStatus) }
    var calendar: Bool { has(Self.calendar) }
    var calendarEmail: Bool { has(Self.calendarEmail) }
    var orgs: Bool { has(Self.orgs) }
    /// Settings → Privacy & filtering is worth a row when any of its controls would be shown.
    var privacy: Bool { has(Self.callAdmission) || has(Self.messageFiltering) || has(Self.callHistory) || receipts }

    private struct Answer: Decodable {
        let features: [String]?
    }

    /// Ask again. A failure is an older postbox, or no network: either way nothing new is on, and
    /// what was on stays on rather than flickering off for one bad request.
    func load(using client: PostboxClient) async {
        guard !Fixtures.enabled else { return }
        do {
            let answer = try await client.send("/v1/capabilities", as: Answer.self)
            enabled = Set(answer.features ?? [])
            loaded = true
        } catch let error as APIError where [400, 404, 501].contains(error.status) {
            enabled = []
            loaded = true
        } catch {
            // Keep what we had.
        }
    }

    func reset() {
        enabled = []
        loaded = false
    }

    #if DEBUG
    /// Fixtures run with every feature on unless `-features=a,b` names a smaller set
    /// (`-features=` with nothing after it is a postbox from before Phase 1b).
    func installFixture() {
        let named = CommandLine.arguments.first { $0.hasPrefix("-features=") }
            .map { String($0.dropFirst("-features=".count)) }
        if let named {
            enabled = Set(named.split(separator: ",").map(String.init))
        } else {
            enabled = [Self.messageFiltering, Self.callAdmission, Self.callHistory, Self.avatars,
                       Self.groups, Self.disappearing, Self.messageEdit, Self.messageReply, Self.receipts, Self.agentStatus]
        }
        loaded = true
    }
    #endif
}
