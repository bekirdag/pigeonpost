import Foundation

@main enum AuthorizationLeaseTests {
    static func main() throws {
        var checks = 0
        func check(_ value: Bool, _ label: String) {
            guard value else { fatalError(label) }
            checks += 1
        }
        let parent = AuthorizationLease()
        let first = AuthorizationLease(parent: parent)
        let other = AuthorizationLease(parent: parent)
        check(first.isValid && other.isValid, "children begin valid")
        check(try first.perform { 42 } == 42, "body result survives")
        first.invalidate()
        check(!first.isValid && other.isValid && parent.isValid, "inbox retirement is local")
        var ran = false
        do { try first.perform { ran = true }; fatalError("retired body ran") } catch is CancellationError {}
        check(!ran, "retired operation has no side effect")
        parent.invalidate()
        check(!other.isValid, "authorization retirement reaches siblings")
        check(!AuthorizationLease(parent: first).isValid, "new child cannot revive a retired ancestor")

        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let file = root.appendingPathComponent("state")
        let authorization = AuthorizationLease()
        let inbox = AuthorizationLease(parent: authorization)
        let entered = DispatchSemaphore(value: 0), release = DispatchSemaphore(value: 0)
        let writerFinished = DispatchSemaphore(value: 0), retiring = DispatchSemaphore(value: 0), erased = DispatchSemaphore(value: 0)
        DispatchQueue.global().async {
            defer { writerFinished.signal() }
            try! inbox.perform {
                entered.signal()
                precondition(release.wait(timeout: .now() + 5) == .success)
                try Data("sealed-state".utf8).write(to: file, options: .atomic)
            }
        }
        check(entered.wait(timeout: .now() + 5) == .success, "save has entered the protected operation")
        DispatchQueue.global().async {
            retiring.signal()
            authorization.invalidate()
            try! FileManager.default.removeItem(at: file)
            erased.signal()
        }
        check(retiring.wait(timeout: .now() + 5) == .success, "retirement requested")
        check(erased.wait(timeout: .now() + 0.05) == .timedOut, "retirement waits for a running save")
        release.signal()
        check(writerFinished.wait(timeout: .now() + 5) == .success, "running save completes")
        check(erased.wait(timeout: .now() + 5) == .success, "erase follows the completed save")
        check(!FileManager.default.fileExists(atPath: file.path), "erase remains final")
        do { try inbox.perform { try Data().write(to: file) }; fatalError("retired save resumed") } catch is CancellationError {}
        check(!FileManager.default.fileExists(atPath: file.path), "late operation cannot recreate erased state")
        let fresh = AuthorizationLease()
        check(fresh.isValid && !inbox.isValid, "new authorization cannot revive an old operation")
        print("Authorization lease: \(checks) checks passed")
    }
}
