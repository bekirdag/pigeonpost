import XCTest

/// Store-screenshot captures of the installed Debug app under `-fixtures -marketing` (the invented
/// world in marketing/store/seed/demo-world.json). Skipped unless PP_MARKETING_SHOTS names an output
/// directory; through xcodebuild set TEST_RUNNER_PP_MARKETING_SHOTS. iPad shots are taken in
/// landscape; the screenshot is redrawn so the rotation is in the pixels, not an orientation tag.
final class MarketingScreenshots: XCTestCase {
    private let app = XCUIApplication(bundleIdentifier: "dev.pigeonpost.inbox")
    private let features = "-features=call_admission,call_history,avatars,groups,disappearing,message_edit,message_reply,receipts,agent_status,calendar,orgs,streaming,meeting_summaries"

    func testMarketingScreenshots() throws {
        let path = ProcessInfo.processInfo.environment["PP_MARKETING_SHOTS"] ?? ""
        try XCTSkipIf(path.isEmpty, "set PP_MARKETING_SHOTS to take marketing screenshots")
        let directory = URL(fileURLWithPath: path)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let pad = UIDevice.current.userInterfaceIdiom == .pad
        XCUIDevice.shared.orientation = pad ? .landscapeLeft : .portrait
        let group = "-open=grp_marketinghikers00000000000"
        let scenes: [(String, [String])] = [
            ("inbox", ["-appearance=light", "-calendar-fixtures"] + (pad ? ["-open=/leo"] : [])),
            ("agent-thread", ["-appearance=dark", "-open=/maya/research-agent"]),
            ("group", ["-appearance=light", group]),
            ("calendar", ["-appearance=light", "-calendar-fixtures", "-sheet=calendar"] + (pad ? ["-open=/leo"] : [])),
            ("teams", ["-appearance=light", "-room-fixtures", "-sheet=org-rooms"] + (pad ? ["-open=/leo"] : [])),
        ]
        // Live streams and meeting notes have native screens only from Apple round three (PR #148):
        // PP_MARKETING_SCENES=live,meeting-notes takes just those, on a build of that branch.
        let only = Set((ProcessInfo.processInfo.environment["PP_MARKETING_SCENES"] ?? "").split(separator: ",").map(String.init))
        let r3: [(String, [String])] = [
            ("live", ["-appearance=dark", "-open=grp_marketingdesign00000000000"]),
            ("meeting-notes", ["-appearance=dark", "-sheet=meeting-notes"] + (pad ? ["-open=/leo"] : [])),
        ]
        let selected = only.isEmpty ? scenes : (scenes + r3).filter { only.contains($0.0) }
        // Scenes already on disk are skipped, so capture.sh can rerun the test after a launch timeout
        // on a loaded machine and pick up where it stopped.
        for (name, extra) in selected where !FileManager.default.fileExists(atPath: directory.appendingPathComponent("\(name).png").path) {
            app.terminate()
            app.launchArguments = ["-fixtures", "-marketing", features] + extra
            app.launch()
            XCTAssertTrue(app.wait(for: .runningForeground, timeout: 60))
            sleep(6)
            if name == "group" && !pad {
                // The thread opens at its top on a phone; bring the newest message above the composer.
                for _ in 0..<3 { app.swipeUp(velocity: .fast) }
                sleep(2)
            }
            if name == "meeting-notes" {
                let row = app.descendants(matching: .any)["meeting-mtg_marketinglaunchsync0000000"].firstMatch
                if row.waitForExistence(timeout: 15) { row.tap() }
                _ = app.descendants(matching: .any)["meetingNoteSummary"].waitForExistence(timeout: 10)
                sleep(2)
            }
            // Draw through UIImage so its orientation tag is applied: the PNG is upright pixels, the
            // way App Store Connect measures it, whichever way the screenshot came back.
            let image = XCUIScreen.main.screenshot().image
            let format = UIGraphicsImageRendererFormat()
            format.scale = image.scale
            format.opaque = true
            let upright = UIGraphicsImageRenderer(size: image.size, format: format).pngData { _ in image.draw(at: .zero) }
            try upright.write(to: directory.appendingPathComponent("\(name).png"))
        }
    }
}
