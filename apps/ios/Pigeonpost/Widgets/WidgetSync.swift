//  Keeping the Home Screen and Lock Screen widgets in step with the app — iPhone and iPad only.
//
//  The app is the only writer. Whenever the acting mailbox's conversations, the account's Nests or
//  the calendar change, a fresh snapshot goes to the keychain group shared with the extension and WidgetKit is asked to
//  redraw; when nothing a widget shows has changed, nothing is written. The extension never sees a
//  token, a key or the network.
//
//  The snapshot belongs to one signed-in account and its acting mailbox, and to nobody else:
//  signing out, the session expiring, or another account appearing removes it before anything new
//  is written.

import Foundation
import SwiftUI
import WidgetKit

@MainActor
final class WidgetSync {
    static let shared = WidgetSync()
    /// "Show message previews in widgets". Off unless the person turns it on.
    static let previewsKey = "ppi_widget_previews"

    enum State: Equatable {
        case signedOut
        /// Signed in, but the mailbox or its inbox is still loading. Keeps the owner's snapshot.
        case waiting(owner: String)
        case ready(WidgetSnapshot)
    }

    private let reload: () -> Void
    private var written: WidgetSnapshot?
    private var knownOwner: String??

    init(reload: @escaping () -> Void = { WidgetCenter.shared.reloadAllTimelines() }) {
        self.reload = reload
    }

    func apply(_ state: State) {
        switch state {
        case .signedOut:
            clear()
        case let .waiting(owner):
            if let stored = storedOwner(), stored != owner { clear() }
        case let .ready(snapshot):
            if let stored = storedOwner(), stored != snapshot.owner { clear() }
            if let written, written.sameContent(as: snapshot) { return }
            var stamped = snapshot
            stamped.generatedAt = Date()
            do {
                try WidgetStore.write(stamped)
                written = stamped
                knownOwner = .some(stamped.owner)
                reload()
            } catch {
                // A widget showing nothing is better than one showing something stale for another
                // account; a failed write leaves nothing behind.
                clear()
            }
        }
    }

    /// Remove the snapshot. Signing out also turns previews back off for whoever signs in next.
    func clear(signingOut: Bool = false) {
        if signingOut { UserDefaults.standard.removeObject(forKey: Self.previewsKey) }
        let hadSomething = written != nil || knownOwner != .some(nil)
        WidgetStore.clear()
        written = nil
        knownOwner = .some(nil)
        if hadSomething { reload() }
    }

    private func storedOwner() -> String? {
        if let knownOwner { return knownOwner }
        let owner = WidgetStore.read()?.owner
        knownOwner = .some(owner)
        return owner
    }
}

/// Where a tapped widget asked to go, until the conversation list is on screen to take it.
@MainActor
@Observable
final class WidgetRouter {
    var pending: WidgetRoute?

    /// True when the URL was a widget link (handled), false to let the other link handlers try it.
    func receive(_ url: URL) -> Bool {
        guard let route = WidgetRoute(url: url) else { return false }
        pending = route
        return true
    }
}

/// Watches what the widgets show and hands each change to `WidgetSync`. On the app's root.
private struct WidgetSnapshots: ViewModifier {
    @Environment(Session.self) private var session
    @Environment(Account.self) private var account
    @Environment(Inbox.self) private var inbox
    @Environment(CalendarStore.self) private var calendar
    @AppStorage(WidgetSync.previewsKey) private var previews = false

    func body(content: Content) -> some View {
        content
            .onChange(of: state, initial: true) { _, state in WidgetSync.shared.apply(state) }
            .onAppear { account.onForget = { WidgetSync.shared.clear(signingOut: true) } }
    }

    private var state: WidgetSync.State {
        guard session.status == .signedIn, let subject = session.subject, !subject.isEmpty else { return .signedOut }
        let owner = WidgetSnapshotBuilder.owner(subject: subject)
        guard let me = account.me, inbox.hasLoaded else { return .waiting(owner: owner) }
        let nestsOn = account.features.nests
        let nests = WidgetSnapshotBuilder.nests(nestsOn ? account.nests.snapshot : nil, available: nestsOn,
                                                usage: NestUsageLedger.shared.usage)
        let agenda = account.features.calendar && calendar.enabled && calendar.mailbox == me.address
            ? WidgetSnapshotBuilder.agenda(calendar.occurrences, now: Date(), zone: calendar.zone) : nil
        return .ready(WidgetSnapshot(
            generatedAt: .distantPast,
            owner: owner,
            mailbox: WidgetSnapshotBuilder.mailbox(me),
            showsPreviews: previews,
            unreadCount: inbox.unreadCount,
            conversations: WidgetSnapshotBuilder.conversations(inbox.conversations, archived: inbox.archived, previews: previews),
            nests: nests,
            agenda: agenda
        ).redacted())
    }
}

extension View {
    func widgetSnapshots() -> some View { modifier(WidgetSnapshots()) }
}
