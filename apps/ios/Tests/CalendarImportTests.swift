import Foundation

@main
struct CalendarImportTests {
    static var checks = 0
    static var failures = 0
    static func check(_ value: Bool, _ label: String) {
        checks += 1
        print((value ? "  ok   " : "  FAIL ") + label)
        if !value { failures += 1 }
    }
    static func refuses(_ label: String, _ body: () throws -> Void) {
        do { try body(); check(false, label) } catch { check(true, label) }
    }
    static func main() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent("pp-calendar-import-" + UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let file = directory.appendingPathComponent("calendar.ics")
        let exact = Data(repeating: 65, count: CalendarImportFile.limit)
        try exact.write(to: file)
        check(try CalendarImportFile.read(file) == exact, "a file at the limit is read without changing its bytes")
        try (exact + Data([66])).write(to: file)
        refuses("an oversized file is rejected") { _ = try CalendarImportFile.read(file) }
        refuses("a directory cannot be read as an import") { _ = try CalendarImportFile.read(directory) }
        refuses("an import never fetches a remote URL") { _ = try CalendarImportFile.read(URL(string: "https://calendar.test/private.ics")!) }

        let preview = CalendarImportPreview(draftId: "draft_a", calendarId: "cal_a", events: [
            CalendarImportEvent(index: 2, importable: true), CalendarImportEvent(index: 0, importable: true),
            CalendarImportEvent(index: 1, importable: false), CalendarImportEvent(index: 3)
        ])
        let confirmation = try CalendarImportConfirmation(preview: preview, selected: [2, 0])
        check(confirmation.draft == "draft_a" && confirmation.calendar == "cal_a" && confirmation.selected == [0, 2],
              "confirmation binds the preview's resolved calendar and canonical selection")
        refuses("an empty selection cannot consume the preview") { _ = try CalendarImportConfirmation(preview: preview, selected: []) }
        refuses("an unimportable entry cannot be selected") { _ = try CalendarImportConfirmation(preview: preview, selected: [1]) }
        refuses("an unknown index cannot be selected") { _ = try CalendarImportConfirmation(preview: preview, selected: [99]) }
        refuses("a missing importability decision cannot be assumed safe") { _ = try CalendarImportConfirmation(preview: preview, selected: [3]) }
        let missing = CalendarImportPreview(draftId: "draft_a", events: preview.events)
        refuses("a missing destination must be resolved before confirmation") { _ = try CalendarImportConfirmation(preview: missing, selected: [0]) }
        let large = CalendarImportPreview(draftId: "draft_a", calendarId: "cal_a", events: (0..<201).map { CalendarImportEvent(index: $0, importable: true) })
        refuses("confirmation cannot exceed the server's selection bound") { _ = try CalendarImportConfirmation(preview: large, selected: Set(0..<201)) }
        print("Calendar import: \(checks) checks, \(failures) failures")
        exit(failures == 0 ? 0 : 1)
    }
}
