//  The small shared pieces: the pills the server's decisions are shown as, and the toast.

import SwiftUI

/// The unread count, centred on the digits rather than on the line box.
///
/// A `Text` centres its line box, and a line box reserves descender space that no digit ever uses —
/// so the plain version of this badge drew the number a point high inside its circle. Proportional
/// figures put it half a point left as well, because "1" is narrow and its side bearings are not
/// symmetric. Monospaced digits fix the horizontal: tabular figures centre each glyph in a uniform
/// advance, which also stops the badge changing width as a count ticks 1 → 7. The vertical is fixed
/// by aligning on the digits' own centre — half a cap height above the baseline — instead of on the
/// box that contains them.
struct UnreadBadge: View {
    let count: Int
    /// On a selected row the two colours swap: navy on navy is a badge nobody can count.
    var inverted: Bool = false

    private static let size: CGFloat = 11
    /// SF's cap height is very close to 0.72 em, and this only has to be right to the pixel.
    private static let capHeight: CGFloat = size * 0.72

    var body: some View {
        Text("\(count)")
            .font(.system(size: Self.size, weight: .semibold))
            .monospacedDigit()
            .foregroundStyle(inverted ? Theme.navy : Color.white)
            .alignmentGuide(VerticalAlignment.center) { d in
                d[.firstTextBaseline] - Self.capHeight / 2
            }
            .padding(.horizontal, 5)
            .frame(minWidth: 18, minHeight: 18)
            .background(inverted ? Color.white : Theme.navy, in: Capsule())
            // Never let a neighbour compress it; the count is two characters at most and all of it
            // matters.
            .fixedSize()
    }
}

/// Open a conversation on its newest message, and stay there.
///
/// Stated as a property of the scroll view rather than as an event. Doing it in `onAppear` is a
/// guess at the timing — that runs before the scroll view has measured its content, so a long
/// conversation opens somewhere in the middle often enough to be a complaint, which is exactly what
/// it was on the Mac.
///
/// Shared because the two clients had the same bug and only one of them had the fix.
struct AnchoredToBottom: ViewModifier {
    @ViewBuilder
    func body(content: Content) -> some View {
        #if os(iOS)
        if #available(iOS 18.0, *) {
            content
                .defaultScrollAnchor(.bottom, for: .initialOffset)
                .defaultScrollAnchor(.bottom, for: .sizeChanges)
                .defaultScrollAnchor(.bottom, for: .alignment)
        } else {
            content.defaultScrollAnchor(.bottom)
        }
        #else
        // Nothing, on purpose. macOS 14's single-argument `defaultScrollAnchor(.bottom)` does not
        // settle a scroll view at its end: it puts the content outside the visible rectangle
        // altogether, so a conversation of any length draws as an empty pane with the composer
        // underneath it. Twelve messages, all present in the model, and not one pixel of them.
        // The Mac scrolls to the floor explicitly after the first layout instead.
        //
        // The three-argument form is what iOS 18 uses above and is the one that behaves; it needs
        // macOS 15, which is newer than this app's deployment target.
        content
        #endif
    }
}

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

/// Mail that arrived somewhere other than the screen you are on, said by the app itself.
///
/// The half of a notification that belongs to an app that is already open. A system banner there is
/// the app being interrupted by an announcement about itself — it covers the navigation bar, it has
/// to be dismissed before the thing underneath can be used, and at its worst it is a banner about
/// the message on screen. This is the same information without any of that: it names who wrote and
/// what they said, it goes on its own after a few seconds, it can be pushed away, and tapping it
/// opens the conversation it is about.
///
/// It is never shown for the conversation being read — `Inbox` drops those before they reach here.
private struct Announcer: ViewModifier {
    @Binding var announcement: Inbox.Announcement?
    let open: (String) -> Void

    func body(content: Content) -> some View {
        content.overlay(alignment: .top) {
            if let announcement {
                line(announcement)
                    .transition(.move(edge: .top).combined(with: .opacity))
                    // Pushed up and away, the gesture the system's own banners take.
                    .gesture(
                        DragGesture(minimumDistance: 12)
                            .onEnded { drag in
                                if drag.translation.height < 0 { self.announcement = nil }
                            }
                    )
                    // Keyed on the id, so a second message restarts the clock rather than
                    // inheriting what was left of the first one's.
                    .task(id: announcement.id) {
                        try? await Task.sleep(nanoseconds: 4_500_000_000)
                        guard !Task.isCancelled else { return }
                        self.announcement = nil
                    }
            }
        }
        .animation(.easeOut(duration: 0.2), value: announcement)
    }

    private func line(_ announcement: Inbox.Announcement) -> some View {
        Button {
            let peer = announcement.peer
            self.announcement = nil
            open(peer)
        } label: {
            HStack(alignment: .top, spacing: 10) {
                Avatar(peer: announcement.peer, size: 30)
                VStack(alignment: .leading, spacing: 2) {
                    Text(announcement.title)
                        .font(.system(size: 13.5, weight: .semibold))
                        .foregroundStyle(Theme.ink)
                        .lineLimit(1)
                    Text(announcement.body)
                        .font(.system(size: 12.5))
                        .foregroundStyle(Theme.body)
                        .lineLimit(2)
                        .multilineTextAlignment(.leading)
                }
                Spacer(minLength: 0)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .frame(maxWidth: 460)
            .background(.regularMaterial, in: RoundedRectangle(cornerRadius: 14))
            .overlay(RoundedRectangle(cornerRadius: 14).stroke(Theme.rule, lineWidth: 1))
            .shadow(color: .black.opacity(0.14), radius: 12, y: 4)
        }
        .buttonStyle(.plain)
        .padding(.horizontal, 12)
        .padding(.top, 6)
        .accessibilityLabel("\(announcement.title): \(announcement.body). Open this conversation")
    }
}

extension View {
    /// See `Announcer`. `open` is given the peer the line was about.
    func announcements(
        _ announcement: Binding<Inbox.Announcement?>,
        open: @escaping (String) -> Void
    ) -> some View {
        modifier(Announcer(announcement: announcement, open: open))
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
                // "Doodle", with the capital, is the name of the imageset. It was spelled
                // "doodle" here, asset lookup is case-sensitive, and SwiftUI answers a name it
                // cannot find with an empty image rather than a complaint — so the pattern was
                // silently absent on the phone for as long as this has existed, leaving the veil
                // sitting on the plain ground and looking exactly like a design decision.
                //
                // The artwork now lives in `Shared/Assets.xcassets`, so the Mac target has it too;
                // before, it was in the iOS catalogue where only one of the two could see it.
                Image("Doodle")
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

extension String {
    /// Written often enough, in both apps, that spelling it out each time is the noise.
    var trimmed: String { trimmingCharacters(in: .whitespacesAndNewlines) }
}
