import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// Paste reads the clipboard only after a user invokes Paste. Keyboard text and IME
/// composition remain UITextView's responsibility; images go into the attachment draft.
struct MessageEditor: UIViewRepresentable {
    @Binding var text: String
    @Binding var focused: Bool
    var enabled: Bool
    let pasteImages: ([NSItemProvider]) -> Void

    func makeCoordinator() -> Coordinator { Coordinator(self) }
    func makeUIView(context: Context) -> PasteTextView {
        let view = PasteTextView()
        view.delegate = context.coordinator
        view.font = Typeface.platform(15)
        view.backgroundColor = .clear
        view.textColor = UIColor(Theme.ink)
        view.tintColor = UIColor(Theme.accent)
        view.textContainerInset = UIEdgeInsets(top: 8, left: 3, bottom: 8, right: 3)
        view.accessibilityLabel = "Write a message"
        view.accessibilityIdentifier = "messageComposer"
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        return view
    }
    func updateUIView(_ view: PasteTextView, context: Context) {
        context.coordinator.parent = self
        view.pasteImages = pasteImages
        view.acceptEdits = enabled
        if view.text != text {
            // A confirmed send clears even pending autocorrection, without replacing the
            // first responder. Ordinary updates respect in-progress IME composition.
            if text.isEmpty { view.unmarkText(); view.text = text }
            else if view.markedTextRange == nil { view.text = text }
        }
        if focused && !view.isFirstResponder && enabled { view.becomeFirstResponder() }
        if !focused && view.isFirstResponder { view.resignFirstResponder() }
    }
    static func dismantleUIView(_ view: PasteTextView, coordinator: Coordinator) {
        coordinator.retired = true
        view.delegate = nil
        view.acceptEdits = false
        view.pasteImages = nil
        view.resignFirstResponder()
    }
    func sizeThatFits(_ proposal: ProposedViewSize, uiView: PasteTextView, context: Context) -> CGSize? {
        guard let width = proposal.width else { return nil }
        return CGSize(width: width, height: min(120, max(34, uiView.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude)).height)))
    }
    final class Coordinator: NSObject, UITextViewDelegate {
        var parent: MessageEditor
        var retired = false
        init(_ parent: MessageEditor) { self.parent = parent }
        func textView(_ view: UITextView, shouldChangeTextIn range: NSRange, replacementText text: String) -> Bool { !retired && parent.enabled }
        func textViewDidChange(_ view: UITextView) {
            guard !retired else { return }
            parent.text = view.text; view.invalidateIntrinsicContentSize()
        }
        func textViewDidBeginEditing(_ view: UITextView) { if !retired { parent.focused = true } }
        func textViewDidEndEditing(_ view: UITextView) { if !retired { parent.focused = false } }
    }
    final class PasteTextView: UITextView {
        var pasteImages: (([NSItemProvider]) -> Void)?
        var acceptEdits = true
        override func canPerformAction(_ action: Selector, withSender sender: Any?) -> Bool {
            if action == #selector(paste(_:)), !acceptEdits { return false }
            if action == #selector(paste(_:)), isEditable, UIPasteboard.general.hasImages { return true }
            return super.canPerformAction(action, withSender: sender)
        }
        override func paste(_ sender: Any?) {
            guard isEditable, acceptEdits else { return }
            if UIPasteboard.general.hasImages {
                let providers = UIPasteboard.general.itemProviders.filter { $0.hasItemConformingToTypeIdentifier(UTType.image.identifier) }
                if !providers.isEmpty { pasteImages?(providers); return }
            }
            super.paste(sender)
        }
    }
}
