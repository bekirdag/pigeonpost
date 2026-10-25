import Foundation
import Observation

/// A stale 401 must not renew a different account's credentials for this lookup.
@MainActor
private final class MeetingLookupTokens: TokenProviding {
    private weak var source: TokenProviding?
    private let current: () -> Bool
    init(source: TokenProviding, current: @escaping () -> Bool) { self.source = source; self.current = current }
    func token() async throws -> String {
        guard current(), !Task.isCancelled, let source else { throw CancellationError() }
        let token = try await source.token()
        guard current(), !Task.isCancelled else { throw CancellationError() }
        return token
    }
    func renew() async throws -> String {
        guard current(), !Task.isCancelled, let source else { throw CancellationError() }
        let token = try await source.renew()
        guard current(), !Task.isCancelled else { throw CancellationError() }
        return token
    }
}

@MainActor
@Observable
final class CalendarMeetingLookup {
    private(set) var information: CalendarMeetingStatus?
    private(set) var loading = false
    private(set) var error: String?
    @ObservationIgnored private let base: URL
    private var operation = UUID()

    init(base: URL = URL(string: "https://inbox.pigeonpost.dev/calls")!) { self.base = base }

    func cancel() {
        operation = UUID(); loading = false; information = nil; error = nil
    }

    func load(code: String, tokens: TokenProviding, current: @escaping () -> Bool) async {
        cancel()
        guard current(), !Task.isCancelled else { return }
        guard let code = CalendarLink.meetingCode(code) else { error = "This meeting link is invalid."; return }
        let visit = operation
        let binding = MeetingLookupTokens(source: tokens) { [weak self] in
            self?.operation == visit && current()
        }
        // PostboxClient intentionally holds its token provider weakly.
        defer { withExtendedLifetime(binding) {} }
        let client = PostboxClient(base: base, tokens: binding)
        loading = true
        defer { if operation == visit { loading = false } }
        do {
            let data = try await client.sendRaw("/v1/meetings/scheduled/" + code, timeout: 20)
            let result = try PostboxClient.decode(CalendarMeetingStatus.self, from: data)
            guard operation == visit, current(), !Task.isCancelled else { return }
            information = result
        } catch is CancellationError {
            // A new context owns the view. It must not inherit a result or error from this one.
        } catch {
            guard operation == visit, current(), !Task.isCancelled else { return }
            self.error = "The meeting could not be loaded. Try again."
        }
    }
}
