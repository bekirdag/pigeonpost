//  Message text that can be partly selected and copied.
//
//  SwiftUI's `Text` with `.textSelection(.enabled)` copies the whole of a text on iOS and only
//  sometimes allows a range on macOS, so a sentence out of an agent's report could not be lifted
//  out without the rest of it. These are the platforms' own read-only text views — `UITextView`
//  and `NSTextView` — sized by SwiftUI like any other view: they never scroll themselves, they take
//  the width they are offered, and they ask for exactly the height their text needs at that width.
//
//  What each platform keeps of the message around it:
//  - iOS: a long press is still the message's menu (Copy, Report spam, Delete). Selection starts
//    with a double tap on a word — or a triple tap for the paragraph — and the handles extend it;
//    the system's edit menu then offers Copy for exactly what is selected.
//  - macOS: click-drag, double- and triple-click select, ⌘C copies. A right-click on a selection
//    is the text's own menu (Copy, Look Up…); a right-click anywhere else is the message's menu.
//  Links stay links on both, and scrolling over the text scrolls the conversation.

import SwiftUI
#if canImport(UIKit)
import UIKit
#elseif canImport(AppKit)
import AppKit
#endif

struct SelectableText: View {
    let text: NSAttributedString
    /// `false` for code: one line per line, as wide as its longest line, inside a sideways scroller.
    var wraps: Bool = true

    var body: some View {
        MeasuredByText(text: text, wraps: wraps) {
            SelectableTextView(text: text, wraps: wraps)
        }
    }
}

/// Sizes its one child from the attributed string alone, never by asking the platform view.
///
/// This is what keeps a conversation landing on its newest message. To scroll a lazy stack to its
/// end, SwiftUI sizes every row above the end, and it can size a `Text` without building it — but a
/// representable's `sizeThatFits` is handed a live view, so rows that were never built fell back to
/// estimates, and the stack ended up scrolled past rows it had placed somewhere else: a blank pane
/// over the composer, with the scroller at the bottom. A `Layout` is pure, so the rows are measured
/// exactly whether or not their text views exist yet.
private struct MeasuredByText: Layout {
    let text: NSAttributedString
    let wraps: Bool

    func makeCache(subviews: Subviews) -> [CGFloat: CGSize] { [:] }

    func sizeThatFits(proposal: ProposedViewSize, subviews: Subviews, cache: inout [CGFloat: CGSize]) -> CGSize {
        let width = proposal.width.flatMap { $0.isFinite ? $0 : nil }
        let key = width ?? -1
        if let known = cache[key] { return known }
        let size = TextMeasure.size(text, width: width, wraps: wraps)
        if cache.count >= 8 { cache.removeAll(keepingCapacity: true) }
        cache[key] = size
        return size
    }

    func placeSubviews(in bounds: CGRect, proposal: ProposedViewSize, subviews: Subviews, cache: inout [CGFloat: CGSize]) {
        for subview in subviews {
            subview.place(at: bounds.origin, anchor: .topLeading, proposal: ProposedViewSize(bounds.size))
        }
    }
}

/// The measurement both platforms share: the height of an attributed string at a width, laid out
/// the way the text views lay it out (no container inset, no line-fragment padding).
enum TextMeasure {
    static func size(_ text: NSAttributedString, width: CGFloat?, wraps: Bool) -> CGSize {
        // A layout probe with no width, or none to speak of, is asking how flexible this is, not
        // how tall it would be in a one-point column. Answering it by wrapping every character
        // onto its own line reported rows tens of thousands of points tall, and a lazy stack that
        // believes that scrolls a conversation far past its own end. Text answers such a probe
        // with its unwrapped size; so does this.
        let usable = width.flatMap { $0 >= 24 ? $0 : nil }
        let limit = wraps ? (usable ?? 100_000) : 100_000
        let bounds = text.boundingRect(
            with: CGSize(width: limit, height: .greatestFiniteMagnitude),
            options: [.usesLineFragmentOrigin, .usesFontLeading],
            context: nil
        )
        let used = CGSize(width: ceil(bounds.width), height: ceil(bounds.height))
        guard wraps else { return used }
        // A wrapping text fills what it is offered — a message card's width — and is measured only
        // for its height. Without an offer it asks for its natural width.
        return CGSize(width: width ?? used.width, height: used.height)
    }
}

/// The last few measurements, so a layout pass that asks the same question several times — as
/// SwiftUI's stacks do — lays the text out once.
final class TextMeasureCache {
    private var text: NSAttributedString?
    private var sizes: [CGFloat: CGSize] = [:]

    func size(_ text: NSAttributedString, width: CGFloat?, wraps: Bool) -> CGSize {
        if self.text !== text, self.text?.isEqual(to: text) != true {
            self.text = text
            sizes.removeAll(keepingCapacity: true)
        }
        let key = width ?? -1
        if let known = sizes[key] { return known }
        let size = TextMeasure.size(text, width: width, wraps: wraps)
        if sizes.count >= 8 { sizes.removeAll(keepingCapacity: true) }
        sizes[key] = size
        return size
    }
}

#if canImport(UIKit)
private struct SelectableTextView: UIViewRepresentable {
    let text: NSAttributedString
    let wraps: Bool

    func makeUIView(context: Context) -> ReadOnlyTextView {
        // TextKit 1, explicitly: `TextMeasure` sizes the view with TextKit 1 metrics, and TextKit 2
        // spaces paragraphs differently — a view drawn by one and sized by the other comes out a
        // line or two taller than its text.
        let view = ReadOnlyTextView(usingTextLayoutManager: false)
        view.isEditable = false
        view.isSelectable = true
        view.isScrollEnabled = false
        view.backgroundColor = .clear
        view.textContainerInset = .zero
        view.textContainer.lineFragmentPadding = 0
        view.textContainer.lineBreakMode = wraps ? .byWordWrapping : .byClipping
        view.dataDetectorTypes = []
        // Link colour and weight come from the text's own attributes.
        view.linkTextAttributes = [:]
        view.tintColor = UIColor(Theme.accent)
        view.adjustsFontForContentSizeCategory = false
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.setContentHuggingPriority(.defaultLow, for: .horizontal)
        view.attributedText = text
        view.yieldLongPress()
        return view
    }

    func updateUIView(_ view: ReadOnlyTextView, context: Context) {
        if view.attributedText != text { view.attributedText = text }
    }

    func makeCoordinator() -> TextMeasureCache { TextMeasureCache() }

    func sizeThatFits(_ proposal: ProposedViewSize, uiView: ReadOnlyTextView, context: Context) -> CGSize? {
        let width = proposal.width.flatMap { $0.isFinite ? $0 : nil }
        return context.coordinator.size(text, width: width, wraps: wraps)
    }
}

final class ReadOnlyTextView: UITextView {
    /// The long press belongs to the message's context menu, which an ancestor's
    /// `UIContextMenuInteraction` presents. A text view's own long-press recognisers would win the
    /// race and turn every long press into a selection, so they step aside; the double-tap, the
    /// triple-tap and the selection handles are unaffected.
    func yieldLongPress() {
        for recognizer in gestureRecognizers ?? [] where recognizer is UILongPressGestureRecognizer {
            recognizer.isEnabled = false
        }
    }

    // Read as what it is — a message's words — rather than as a text area. VoiceOver reads it as it
    // read the `Text` this replaced, and UI tests find it by its words as they did then.
    override var accessibilityTraits: UIAccessibilityTraits {
        get { .staticText }
        set {}
    }
    override var accessibilityLabel: String? {
        get { attributedText?.string }
        set {}
    }
    override var accessibilityValue: String? {
        get { nil }
        set {}
    }

    override func didMoveToWindow() {
        super.didMoveToWindow()
        // Text interactions are installed lazily, the first time the view joins a window.
        yieldLongPress()
    }

    /// Losing focus — another message, the composer, a scroll — clears the highlight, so a stale
    /// selection does not linger on a message the person has moved on from.
    override func resignFirstResponder() -> Bool {
        let resigned = super.resignFirstResponder()
        if resigned { selectedTextRange = nil }
        return resigned
    }
}
#elseif canImport(AppKit)
private struct SelectableTextView: NSViewRepresentable {
    let text: NSAttributedString
    let wraps: Bool

    func makeNSView(context: Context) -> ReadOnlyTextView {
        // TextKit 1, explicitly: `TextMeasure` sizes the view with TextKit 1 metrics, and TextKit 2
        // spaces paragraphs differently — a view drawn by one and sized by the other comes out a
        // line or two taller than its text.
        let storage = NSTextStorage()
        let layout = NSLayoutManager()
        storage.addLayoutManager(layout)
        let container = NSTextContainer(size: NSSize(width: 0, height: CGFloat.greatestFiniteMagnitude))
        layout.addTextContainer(container)
        let view = ReadOnlyTextView(frame: .zero, textContainer: container)
        view.isEditable = false
        view.isSelectable = true
        view.isRichText = true
        view.drawsBackground = false
        view.textContainerInset = .zero
        view.textContainer?.lineFragmentPadding = 0
        view.textContainer?.widthTracksTextView = wraps
        view.textContainer?.containerSize = NSSize(width: wraps ? 0 : 100_000, height: CGFloat.greatestFiniteMagnitude)
        view.isVerticallyResizable = false
        view.isHorizontallyResizable = false
        view.linkTextAttributes = [.cursor: NSCursor.pointingHand]
        view.selectedTextAttributes = [.backgroundColor: NSColor(Theme.accent).withAlphaComponent(0.32)]
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.textStorage?.setAttributedString(text)
        return view
    }

    func updateNSView(_ view: ReadOnlyTextView, context: Context) {
        if view.textStorage?.isEqual(to: text) == false {
            view.textStorage?.setAttributedString(text)
        }
    }

    func makeCoordinator() -> TextMeasureCache { TextMeasureCache() }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView: ReadOnlyTextView, context: Context) -> CGSize? {
        let width = proposal.width.flatMap { $0.isFinite ? $0 : nil }
        return context.coordinator.size(text, width: width, wraps: wraps)
    }
}

final class ReadOnlyTextView: NSTextView {
    override var intrinsicContentSize: NSSize { NSSize(width: NSView.noIntrinsicMetric, height: NSView.noIntrinsicMetric) }

    /// A selection's own menu when there is a selection under the pointer; otherwise the message's
    /// menu (Copy, Report spam, Delete), which SwiftUI attaches to the card around this view.
    override func menu(for event: NSEvent) -> NSMenu? {
        let point = convert(event.locationInWindow, from: nil)
        let index = characterIndexForInsertion(at: point)
        let range = selectedRange()
        guard range.length > 0, NSLocationInRange(index, range) || index == NSMaxRange(range) else { return nil }
        return super.menu(for: event)
    }

    override func rightMouseDown(with event: NSEvent) {
        if menu(for: event) == nil {
            nextResponder?.rightMouseDown(with: event)
        } else {
            super.rightMouseDown(with: event)
        }
    }

    /// Clicking elsewhere ends the selection, as it does in any other read-only text.
    override func resignFirstResponder() -> Bool {
        let resigned = super.resignFirstResponder()
        if resigned { setSelectedRange(NSRange(location: selectedRange().location, length: 0)) }
        return resigned
    }
}
#endif
