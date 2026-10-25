import SwiftUI
#if os(iOS)
import SafariServices
#else
import AppKit
#endif

/// Guest capabilities remain in the existing browser flow, separate from member call credentials.
struct CalendarGuestLinkBrowser: View {
    let url: URL
    @Environment(\.dismiss) private var dismiss
    @State private var failed = false
    var body: some View {
        if case .guest(let safeURL) = CalendarLink(url: url) {
            #if os(iOS)
            GuestSafari(url: safeURL)
            #else
            NavigationStack {
                VStack(spacing: 18) {
                    Image(systemName: "calendar").font(.system(size: 32)).foregroundStyle(Theme.navy)
                    Text(safeURL.path == "/m/u" ? "Calendar notifications" : "Personal calendar invitation").font(.geist(20, .semibold))
                    Text(safeURL.path == "/m/u" ? "Choose your invitation notification settings in your browser." : "View the invitation and choose your answer in your browser.").foregroundStyle(Theme.secondary)
                    Button(safeURL.path == "/m/u" ? "Open notification settings" : "Open invitation") {
                        if NSWorkspace.shared.open(safeURL) { dismiss() } else { failed = true }
                    }.buttonStyle(.borderedProminent).tint(Theme.navy)
                    if failed { Text("The browser could not be opened.").foregroundStyle(Color.red) }
                }.padding(32)
                .toolbar { ToolbarItem(placement: .cancellationAction) { Button("Done") { dismiss() } } }
            }.frame(width: 560, height: 340)
            #endif
        } else { ContentUnavailableView("Invalid invitation link", systemImage: "calendar.badge.exclamationmark") }
    }
}

#if os(iOS)
private struct GuestSafari: UIViewControllerRepresentable {
    let url: URL
    @Environment(\.dismiss) private var dismiss
    func makeCoordinator() -> Coordinator { Coordinator(close: { dismiss() }) }
    func makeUIViewController(context: Context) -> SFSafariViewController {
        let view = SFSafariViewController(url: url)
        view.delegate = context.coordinator
        view.dismissButtonStyle = .close
        return view
    }
    func updateUIViewController(_ view: SFSafariViewController, context: Context) {}
    final class Coordinator: NSObject, SFSafariViewControllerDelegate {
        let close: () -> Void
        init(close: @escaping () -> Void) { self.close = close }
        func safariViewControllerDidFinish(_ controller: SFSafariViewController) { close() }
    }
}
#endif
