// Message text can be partly selected and copied on the Mac.
//
// The real MarkdownText / SelectableText in a scrolling lazy stack, driven by mouse events posted to
// this process's own window — the real pointer never moves and the general pasteboard is never
// touched (Copy is checked against a private pasteboard). Run with Tests/mac-selection.sh; an
// optional argument is a path to write a PNG of the window with a selection drawn.
import AppKit
import SwiftUI

@MainActor func find<T: NSView>(_ type: T.Type, in view: NSView) -> [T] {
    var out: [T] = []
    if let v = view as? T { out.append(v) }
    for sub in view.subviews { out += find(type, in: sub) }
    return out
}

@MainActor var checks = 0, failures = 0
@MainActor func check(_ ok: Bool, _ what: String) { checks += 1; if !ok { failures += 1 }; print(ok ? "  ok  " : "  FAIL", what) }

@MainActor func post(_ type: NSEvent.EventType, _ p: NSPoint, _ w: NSWindow, clicks: Int = 1, flags: NSEvent.ModifierFlags = []) {
    let e = NSEvent.mouseEvent(with: type, location: p, modifierFlags: flags, timestamp: ProcessInfo.processInfo.systemUptime,
                               windowNumber: w.windowNumber, context: nil, eventNumber: 0, clickCount: clicks, pressure: 1)!
    NSApp.postEvent(e, atStart: false)
}

@main @MainActor struct Harness {
    static func main() {
        let app = NSApplication.shared
        app.setActivationPolicy(.regular)
        let body = "The fixture listener was **non-blocking**, which macOS inherits onto the accepted socket.\n\n- set_nonblocking in serve\n- one test added"
        let root = ScrollView {
            LazyVStack(spacing: 16) {
                ForEach(0..<12) { i in
                    MarkdownText(raw: i == 1 ? body : "Filler message \(i)")
                        .padding(12)
                        .contextMenu { Button("Delete") {} }
                }
            }.padding(20)
        }.frame(width: 520, height: 420)
        let host = NSHostingView(rootView: root)
        let window = NSWindow(contentRect: NSRect(x: 200, y: 200, width: 520, height: 420), styleMask: [.titled], backing: .buffered, defer: false)
        window.contentView = host
        window.acceptsMouseMovedEvents = true
        window.makeKeyAndOrderFront(nil)
        app.activate(ignoringOtherApps: true)
        Task { @MainActor in
            try? await Task.sleep(for: .seconds(1.5))
            window.makeFirstResponder(nil)
            // At rest the words are drawn by SwiftUI: a lazy stack of AppKit views does not build
            // the rows a jump lands on (see SelectableText), so none may exist yet.
            check(find(ReadOnlyTextView.self, in: host).isEmpty, "at rest, no message holds an AppKit text view")
            // Pointing at the second message brings its text view in.
            // Hovering is what brings a text view in, and SwiftUI reports a hover only for the real
            // pointer, which this test does not move. The second window keeps its views in place.
            let live = NSHostingView(rootView: root.environment(\.selectableTextStaysLive, true))
            let liveWindow = NSWindow(contentRect: NSRect(x: 260, y: 160, width: 520, height: 420), styleMask: [.titled],
                                      backing: .buffered, defer: false)
            liveWindow.contentView = live
            liveWindow.makeKeyAndOrderFront(nil)
            try? await Task.sleep(for: .seconds(1.5))
            let window = liveWindow, host = live
            window.makeFirstResponder(nil)
            let views = find(ReadOnlyTextView.self, in: host)
            check(views.count >= 3, "message prose becomes read-only NSTextViews (\(views.count))")
            guard let tv = views.first(where: { $0.string.hasPrefix("The fixture") }) else { finish(); return }
            // One layout pass for the view that just took the canvas's place.
            try? await Task.sleep(for: .milliseconds(300))
            check(tv.frame.height > 40 && tv.isSelectable && !tv.isEditable, "text view is selectable, read-only and sized (\(tv.frame.size))")
            // Lay out and find the window points of characters 4 ("fixture") and 20 ("was").
            let lm = tv.layoutManager!, tc = tv.textContainer!
            lm.ensureLayout(for: tc)
            @MainActor func point(_ index: Int) -> NSPoint {
                let glyphs = lm.glyphRange(forCharacterRange: NSRange(location: index, length: 1), actualCharacterRange: nil)
                let rect = lm.boundingRect(forGlyphRange: glyphs, in: tc)
                return tv.convert(NSPoint(x: rect.minX + 0.5, y: rect.midY), to: nil)
            }
            let from = point(4), to = point(24)
            // Double-click "fixture": one word of the sentence, not the sentence. (A drag uses
            // the same selection machinery, but AppKit's drag tracking follows the real pointer,
            // which this check deliberately does not move.)
            post(.leftMouseDown, from, window, clicks: 2); post(.leftMouseUp, from, window, clicks: 2)
            try? await Task.sleep(for: .milliseconds(300))
            try? await Task.sleep(for: .milliseconds(400))
            let range = tv.selectedRange()
            let selected = (tv.string as NSString).substring(with: range)
            check(range.length > 0 && range.length < tv.string.count, "double-click selects part of the message: \"\(selected)\" \(range)")
            check(selected == "fixture", "the selection is exactly the chosen word")
            check(tv.validateUserInterfaceItem(NSMenuItem(title: "Copy", action: #selector(NSText.copy(_:)), keyEquivalent: "c")),
                  "Copy (⌘C) is enabled for the selection")
            let board = NSPasteboard(name: NSPasteboard.Name("dev.pigeonpost.selection-check"))
            board.clearContents()
            // What Copy (⌘C) does: write the selection in the text view's own writable types.
            let wrote = tv.writeSelection(to: board, types: tv.writablePasteboardTypes)
            check(wrote && board.string(forType: .string) == "fixture",
                  "copy writes only the selected words: \"\(board.string(forType: .string) ?? "nil")\"")
            board.releaseGlobally()
            // Double-click selects one word.
            let word = point(30)
            post(.leftMouseDown, word, window, clicks: 2); post(.leftMouseUp, word, window, clicks: 2)
            try? await Task.sleep(for: .milliseconds(300))
            let wordRange = tv.selectedRange()
            check((tv.string as NSString).substring(with: wordRange) == "blocking", "double-click selects a word: \"\((tv.string as NSString).substring(with: wordRange))\"")
            // Right-click on the selection is the text's menu; with no selection it is the message's.
            let rightDown = NSEvent.mouseEvent(with: .rightMouseDown, location: word, modifierFlags: [], timestamp: 0,
                                               windowNumber: window.windowNumber, context: nil, eventNumber: 0, clickCount: 1, pressure: 1)!
            check(tv.menu(for: rightDown) != nil, "right-click on a selection offers the text menu")
            tv.setSelectedRange(NSRange(location: 0, length: 0))
            check(tv.menu(for: rightDown) == nil, "right-click without a selection falls through to the message menu")
            // Scrolling over the text still scrolls the conversation.
            let scroll = find(NSScrollView.self, in: host).first
            let before = scroll?.contentView.bounds.origin.y ?? -1
            if let cg = CGEvent(scrollWheelEvent2Source: nil, units: .pixel, wheelCount: 1, wheel1: -60, wheel2: 0, wheel3: 0),
               let e = NSEvent(cgEvent: cg) {
                tv.scrollWheel(with: e)
            }
            try? await Task.sleep(for: .milliseconds(300))
            let after = scroll?.contentView.bounds.origin.y ?? -1
            check(after != before, "a scroll-wheel event over the text scrolls the list (\(before) -> \(after))")
            let image = host.bitmapImageRepForCachingDisplay(in: host.bounds)!
            tv.setSelectedRange(NSRange(location: 4, length: 20)); tv.display()
            host.cacheDisplay(in: host.bounds, to: image)
            if CommandLine.arguments.count > 1 {
                try? image.representation(using: .png, properties: [:])?.write(to: URL(fileURLWithPath: CommandLine.arguments[1]))
            }
            finish()
        }
        app.run()
    }

    static func finish() {
        print("Mac selection: \(checks) checks, \(failures) failures")
        exit(failures == 0 ? 0 : 1)
    }
}
