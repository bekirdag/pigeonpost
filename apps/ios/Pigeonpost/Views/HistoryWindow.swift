import Foundation

/// Display paging over the inbox snapshot. IDs are newest first, like the native table.
/// While reading history, incoming mail is held outside the window until returning to latest.
struct HistoryWindow {
    let pageSize: Int
    private(set) var ids: [String] = []

    init(pageSize: Int = 10) {
        precondition(pageSize > 0)
        self.pageSize = pageSize
    }

    mutating func update(_ chronologicalIDs: [String], followingLatest: Bool) {
        if followingLatest || ids.isEmpty {
            ids = Array(chronologicalIDs.suffix(pageSize).reversed())
        } else {
            let visible = Set(ids)
            ids = chronologicalIDs.reversed().filter { visible.contains($0) }
        }
    }

    @discardableResult
    mutating func loadOlder(_ chronologicalIDs: [String]) -> Int {
        guard let oldest = ids.last, let end = chronologicalIDs.firstIndex(of: oldest), end > 0 else { return 0 }
        let older = chronologicalIDs[max(0, end - pageSize)..<end]
        ids.append(contentsOf: older.reversed())
        return older.count
    }

    func hasOlder(in chronologicalIDs: [String]) -> Bool {
        guard let oldest = ids.last, let end = chronologicalIDs.firstIndex(of: oldest) else { return false }
        return end > 0
    }
}
