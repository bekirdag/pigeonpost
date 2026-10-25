import Foundation

enum CalendarImportProblem: LocalizedError {
    case file, tooLarge, destination, selection
    var errorDescription: String? {
        switch self {
        case .file: return "Choose an iCalendar file."
        case .tooLarge: return "Calendar files must be 512 KB or smaller."
        case .destination: return "Preview this file again to choose an available calendar."
        case .selection: return "Select at least one event that can be imported."
        }
    }
}

enum CalendarImportFile {
    static let limit = 512 * 1024
    static func read(_ url: URL) throws -> Data {
        guard url.isFileURL else { throw CalendarImportProblem.file }
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        let resource = try url.resourceValues(forKeys: [.isRegularFileKey, .fileSizeKey])
        guard resource.isRegularFile == true else { throw CalendarImportProblem.file }
        guard (resource.fileSize ?? 0) <= limit else { throw CalendarImportProblem.tooLarge }
        let file = try FileHandle(forReadingFrom: url)
        defer { try? file.close() }
        var data = Data()
        // The file can grow after checking its metadata. Read one extra byte to enforce the bound.
        while data.count <= limit {
            guard let chunk = try file.read(upToCount: min(64 * 1024, limit + 1 - data.count)), !chunk.isEmpty else { break }
            data.append(chunk)
        }
        guard data.count <= limit else { throw CalendarImportProblem.tooLarge }
        return data
    }
}

/// Freeze before the first confirmation. A lost response must retry these exact semantics.
struct CalendarImportConfirmation: Equatable {
    let draft: String
    let calendar: String
    let selected: [Int]

    init(preview: CalendarImportPreview, selected: Set<Int>) throws {
        guard let calendar = preview.calendarId, !calendar.isEmpty else { throw CalendarImportProblem.destination }
        let allowed = Set((preview.events ?? []).filter { $0.importable == true }.map(\.index))
        guard !selected.isEmpty, selected.count <= 200, selected.isSubset(of: allowed) else { throw CalendarImportProblem.selection }
        draft = preview.draftId; self.calendar = calendar; self.selected = selected.sorted()
    }
}
