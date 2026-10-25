import Foundation

enum AuthError: Error { case sessionExpired }
@MainActor final class Session {
    var renewals = 0
    private var access = "fixture-member"
    func token() async throws -> String { access }
    func renew() async throws -> String { renewals += 1; access = "fixture-renewed"; return access }
}

private final class MeetingTransport: URLProtocol, @unchecked Sendable {
    @MainActor static var pending: [MeetingTransport] = []
    @MainActor static var count = 0
    private let lock = NSLock()
    private var stopped = false
    override class func canInit(with request: URLRequest) -> Bool { request.url?.host == "meeting-fixture.invalid" }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }
    override func startLoading() {
        Task { @MainActor in Self.pending.append(self); Self.count += 1 }
    }
    override func stopLoading() { lock.lock(); stopped = true; lock.unlock() }
    @MainActor func complete(_ status: Int, _ data: Data) {
        lock.lock(); let deliver = !stopped; stopped = true; lock.unlock()
        guard deliver else { return }
        let response = HTTPURLResponse(url: request.url!, statusCode: status, httpVersion: nil,
                                       headerFields: ["content-type": "application/json"])!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: data)
        client?.urlProtocolDidFinishLoading(self)
    }
}

@main enum CalendarMeetingLookupTests {
    @MainActor private final class Gate { var current = true }
    @MainActor static func main() async throws {
        URLProtocol.registerClass(MeetingTransport.self)
        var checks = 0
        func check(_ value: Bool, _ label: String) { precondition(value, label); checks += 1 }
        func request() async throws -> MeetingTransport {
            for _ in 0..<400 {
                if !MeetingTransport.pending.isEmpty { return MeetingTransport.pending.removeFirst() }
                try await Task.sleep(for: .milliseconds(5))
            }
            preconditionFailure("Expected synthetic scheduled-status request")
        }
        func answer(_ id: String) throws -> Data {
            try JSONSerialization.data(withJSONObject: ["state": "open", "occurrences": [[
                "occurrence_id": id, "opens_at": 100, "starts_at": 200, "ends_at": 300, "closes_at": 400]]])
        }
        let unauthorized = Data("{\"error\":\"unauthorized\"}".utf8)
        let tokens = Session()
        let gate = Gate()
        let model = CalendarMeetingLookup(base: URL(string: "https://meeting-fixture.invalid/calls")!)
        let code = "ABCDEFGHJK"

        var task = Task { await model.load(code: code, tokens: tokens, current: { gate.current }) }
        let initial = try await request()
        check(initial.request.url?.path == "/calls/v1/meetings/scheduled/ABCDEFGHJK", "static origin/path")
        check(initial.request.httpMethod == "GET" && initial.request.httpBody == nil, "read-only preview")
        check(initial.request.value(forHTTPHeaderField: "authorization") == "Bearer fixture-member", "member bearer")
        check(initial.request.url?.query == nil, "no credentials in URL")
        check(model.loading, "loading")
        initial.complete(200, try answer("occ_first")); await task.value
        check(model.information?.choices.first?.id == "occ_first", "wire response")
        check(!model.loading && model.error == nil, "completion")

        let count = MeetingTransport.count
        await model.load(code: "bad/code", tokens: tokens, current: { gate.current })
        check(MeetingTransport.count == count, "invalid codes never request")
        check(model.information == nil && model.error != nil, "invalid code clears prior information")

        task = Task { await model.load(code: code, tokens: tokens, current: { gate.current }) }
        let expired = try await request(); expired.complete(401, unauthorized)
        let renewed = try await request()
        check(renewed.request.value(forHTTPHeaderField: "authorization") == "Bearer fixture-renewed", "401 uses renewed token")
        renewed.complete(200, try answer("occ_renewed")); await task.value
        check(tokens.renewals == 1 && model.information?.choices.first?.id == "occ_renewed", "one ordinary renewal")

        task = Task { await model.load(code: code, tokens: tokens, current: { gate.current }) }
        let stale401 = try await request(); gate.current = false
        stale401.complete(401, unauthorized); await task.value
        check(tokens.renewals == 1, "stale context cannot renew")
        check(model.information == nil && model.error == nil && !model.loading, "stale 401 stays empty")
        check(MeetingTransport.pending.isEmpty, "stale 401 has no retry")
        gate.current = true

        let older = Task { await model.load(code: code, tokens: tokens, current: { gate.current }) }
        let oldRequest = try await request()
        let newer = Task { await model.load(code: "GHJKMNPQRS", tokens: tokens, current: { gate.current }) }
        let newRequest = try await request()
        newRequest.complete(200, try answer("occ_new")); await newer.value
        oldRequest.complete(200, try answer("occ_old")); await older.value
        check(model.information?.choices.first?.id == "occ_new", "late success cannot replace new lookup")
        check(!model.loading && model.error == nil, "late success cannot change new status")

        task = Task { await model.load(code: code, tokens: tokens, current: { gate.current }) }
        let signout = try await request(); gate.current = false
        signout.complete(200, try answer("occ_private")); await task.value
        check(model.information == nil && model.error == nil, "identity loss discards private result")
        gate.current = true

        task = Task { await model.load(code: code, tokens: tokens, current: { gate.current }) }
        let cancelled = try await request(); model.cancel()
        cancelled.complete(401, unauthorized); await task.value
        check(tokens.renewals == 1 && model.information == nil && !model.loading, "cancel retires renewal and result")

        task = Task { await model.load(code: code, tokens: tokens, current: { gate.current }) }
        let denied = try await request()
        denied.complete(403, Data("{\"error\":\"forbidden\",\"detail\":\"untrusted private server text\"}".utf8)); await task.value
        check(model.information == nil && model.error == "The meeting could not be loaded. Try again.", "permission failure clears data without echoing server text")
        check(MeetingTransport.pending.isEmpty, "all transport requests completed")
        print("Calendar meeting lookup: \(checks) checks passed")
    }
}
