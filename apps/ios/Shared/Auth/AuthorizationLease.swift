import Foundation

/// Revocable permission for synchronous local work. Never hold this lease across an await.
/// An operation takes ancestor locks before its own; retirement waits for an already running
/// operation, so erase-after-retirement cannot be undone by a late writer.
final class AuthorizationLease: @unchecked Sendable {
    private let parent: AuthorizationLease?
    private let lock = NSRecursiveLock()
    private var active = true

    init(parent: AuthorizationLease? = nil) { self.parent = parent }

    var isValid: Bool { (try? perform { true }) == true }

    func check() throws { try perform {} }

    func perform<T>(_ body: () throws -> T) throws -> T {
        if let parent { return try parent.perform { try own(body) } }
        return try own(body)
    }

    private func own<T>(_ body: () throws -> T) throws -> T {
        lock.lock()
        defer { lock.unlock() }
        guard active else { throw CancellationError() }
        return try body()
    }

    func invalidate() {
        lock.lock()
        active = false
        lock.unlock()
    }
}
