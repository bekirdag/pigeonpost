//  The small shared pieces: the pills the server's decisions are shown as, and the toast.

import SwiftUI

struct PillView: View {
    enum Kind { case held, auto, blocked }

    let text: String
    let kind: Kind

    var body: some View {
        Text(text.uppercased())
            .font(.system(size: 10, weight: .semibold))
            .kerning(0.3)
            .foregroundStyle(foreground)
            .padding(.horizontal, 6)
            .padding(.vertical, 2)
            .background(fill, in: RoundedRectangle(cornerRadius: 5))
            .overlay(RoundedRectangle(cornerRadius: 5).stroke(stroke, lineWidth: 1))
    }

    private var foreground: Color {
        switch kind {
        case .held: return Theme.Pill.heldText
        case .auto: return Theme.Pill.autoText
        case .blocked: return Theme.Pill.blockedText
        }
    }

    private var fill: Color {
        switch kind {
        case .held: return Theme.Pill.heldFill
        case .auto: return Theme.Pill.autoFill
        case .blocked: return Theme.Pill.blockedFill
        }
    }

    private var stroke: Color {
        switch kind {
        case .held: return Theme.Pill.heldStroke
        case .auto: return Theme.Pill.autoStroke
        case .blocked: return Theme.Pill.blockedStroke
        }
    }
}

/// A line of text that appears, is read, and goes. Anything worth keeping belongs on the screen it
/// happened on, not here.
private struct Toast: ViewModifier {
    @Binding var message: String?

    func body(content: Content) -> some View {
        content.overlay(alignment: .bottom) {
            if let message {
                Text(message)
                    .font(.system(size: 13.5))
                    .foregroundStyle(.white)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    .background(Theme.toastFill, in: RoundedRectangle(cornerRadius: 10))
                    .padding(.horizontal, 20)
                    .padding(.bottom, 28)
                    .transition(.move(edge: .bottom).combined(with: .opacity))
                    .task(id: message) {
                        try? await Task.sleep(nanoseconds: 5_200_000_000)
                        guard !Task.isCancelled else { return }
                        self.message = nil
                    }
            }
        }
        .animation(.easeOut(duration: 0.18), value: message)
    }
}

extension View {
    func toast(_ message: Binding<String?>) -> some View {
        modifier(Toast(message: message))
    }
}

/// The tiled doodle behind a conversation, the same one the web inbox uses.
///
/// Three layers, bottom to top: the page colour, the artwork tiled over it, and the veil that sets
/// how strongly the pattern reads. The tile is 300pt, which is what `app.css` asks for in CSS
/// pixels — the artwork is 600×600 and is declared as an @2x asset, so the two clients draw the
/// pattern at the same size rather than one of them at twice the other.
///
/// It belongs behind the scroll view rather than inside it. Pinned to the content, the pattern
/// slides away as a conversation is read; pinned to the element, it stays put underneath. The web
/// app makes the same choice with `background-attachment: scroll` and says why.
struct DoodleBackground: View {
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        Theme.wash
            .overlay {
                // By name rather than through the generated symbol: the asset lives in the iOS
                // target's catalogue, and a target that does not carry it should get the plain
                // ground instead of failing to compile. The pattern is decoration; the colour
                // underneath is the part that matters.
                Image("doodle")
                    .resizable(resizingMode: .tile)
                    // The artwork is near-black strokes on transparency, which is a pattern only on
                    // paper. Inverted, the same strokes are the light-on-dark version of themselves
                    // rather than a smudge nobody can see.
                    .colorInvert(when: scheme == .dark)
            }
            .overlay(Theme.doodleVeil)
            .ignoresSafeArea()
            .accessibilityHidden(true)
    }
}


extension View {
    /// `colorInvert()` has no conditional form, and branching the whole view in a `ViewBuilder`
    /// makes SwiftUI rebuild the tiled image on every appearance change.
    @ViewBuilder
    func colorInvert(when condition: Bool) -> some View {
        if condition { colorInvert() } else { self }
    }
}
