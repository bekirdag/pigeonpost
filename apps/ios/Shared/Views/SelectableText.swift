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
//
//  On the Mac the text view exists only while it is being used. A lazy stack whose rows hold
//  AppKit views does not build the rows at an offset it jumps to — opening a long conversation
//  showed a blank pane with the scroller at the bottom — so at rest the words are drawn by a SwiftUI
//  `Canvas`, with the same TextKit layout the text view uses, and the `NSTextView` takes its place
//  when the pointer comes over the text, staying while it holds a selection. The two draw the same
//  glyphs in the same places, so the swap is invisible.

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

    #if canImport(UIKit)
    var body: some View {
        MeasuredByText(text: text, wraps: wraps) {
            SelectableTextView(text: text, wraps: wraps)
        }
    }
    #elseif canImport(AppKit)
    @State private var hovering = false
    @State private var selecting = false
    @Environment(\.selectableTextStaysLive) private var staysLive

    var body: some View {
        MeasuredByText(text: text, wraps: wraps) {
            if hovering || selecting || staysLive {
                SelectableTextView(text: text, wraps: wraps, selecting: $selecting)
            } else {
                TextCanvas(text: text, wraps: wraps)
            }
        }
        .onHover { hovering = $0 }
    }
    #endif
}

extension EnvironmentValues {
    /// Keep the Mac's text views in place rather than swapping them in on hover. For the selection
    /// test, which cannot move the real pointer and so cannot make SwiftUI report a hover.
    @Entry var selectableTextStaysLive = false
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
        if view.attributedText != text {
            // Dynamic theme colours compare by identity, so an equal text can arrive as unequal;
            // keep the selection when the words themselves have not changed.
            let unchanged = view.attributedText?.string == text.string
            let selection = view.selectedRange
            view.attributedText = text
            if unchanged { view.selectedRange = selection }
        }
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
    /// Whether this text holds a selection, so the view stays after the pointer leaves.
    @Binding var selecting: Bool

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
        view.delegate = context.coordinator
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
        context.coordinator.selecting = $selecting
        if view.textStorage?.isEqual(to: text) == false {
            // The theme's colours are dynamic and compare by identity, so a re-render hands over
            // an equal text that is not `isEqual`. Re-applying it must not drop what the person
            // has selected in the same words.
            let unchanged = view.string == text.string
            let selection = view.selectedRanges
            view.textStorage?.setAttributedString(text)
            if unchanged { view.selectedRanges = selection }
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator(selecting: $selecting) }

    func sizeThatFits(_ proposal: ProposedViewSize, nsView: ReadOnlyTextView, context: Context) -> CGSize? {
        let width = proposal.width.flatMap { $0.isFinite ? $0 : nil }
        return context.coordinator.measure.size(text, width: width, wraps: wraps)
    }

    static func dismantleNSView(_ view: ReadOnlyTextView, coordinator: Coordinator) {
        view.delegate = nil
    }

    final class Coordinator: NSObject, NSTextViewDelegate {
        var selecting: Binding<Bool>
        let measure = TextMeasureCache()
        init(selecting: Binding<Bool>) { self.selecting = selecting }

        func textViewDidChangeSelection(_ notification: Notification) {
            guard let view = notification.object as? NSTextView else { return }
            let has = view.selectedRange().length > 0
            let binding = selecting
            // After the event that changed it, not inside it: a state change re-renders the
            // message while AppKit is still tracking the click.
            DispatchQueue.main.async { if binding.wrappedValue != has { binding.wrappedValue = has } }
        }
    }
}

/// The words at rest: drawn by SwiftUI, with exactly the TextKit 1 layout the text view uses, so
/// a lazy stack of messages holds no AppKit views until one is pointed at.
private struct TextCanvas: View {
    let text: NSAttributedString
    let wraps: Bool
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        Canvas { context, size in
            context.withCGContext { cg in
                let appearance = NSAppearance(named: scheme == .dark ? .darkAqua : .aqua) ?? NSAppearance.currentDrawing()
                appearance.performAsCurrentDrawingAppearance {
                    NSGraphicsContext.saveGraphicsState()
                    NSGraphicsContext.current = NSGraphicsContext(cgContext: cg, flipped: true)
                    let storage = NSTextStorage(attributedString: text)
                    let layout = NSLayoutManager()
                    storage.addLayoutManager(layout)
                    let container = NSTextContainer(size: NSSize(width: wraps ? size.width : 100_000,
                                                                 height: CGFloat.greatestFiniteMagnitude))
                    container.lineFragmentPadding = 0
                    layout.addTextContainer(container)
                    let glyphs = layout.glyphRange(for: container)
                    layout.drawBackground(forGlyphRange: glyphs, at: .zero)
                    layout.drawGlyphs(forGlyphRange: glyphs, at: .zero)
                    NSGraphicsContext.restoreGraphicsState()
                }
            }
        }
        // Read as the words, as the text view and the `Text` before it were.
        .accessibilityElement()
        .accessibilityLabel(text.string)
        .accessibilityAddTraits(.isStaticText)
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
