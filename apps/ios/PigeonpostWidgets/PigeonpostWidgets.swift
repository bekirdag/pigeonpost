//  The Pigeonpost widget extension (iPhone and iPad; never the Mac).
//
//  Everything here reads the snapshot the app wrote to their shared keychain group (WidgetShared/WidgetStore.swift)
//  and nothing else: no token, no key, no crypto state, no network. A widget that finds no snapshot
//  — signed out, or the app not opened since installing — says so and opens the app when tapped.

import SwiftUI
import WidgetKit
import PigeonpostUI

@main
struct PigeonpostWidgets: WidgetBundle {
    var body: some Widget {
        InboxWidget()
        QuickActionsWidget()
        NestsWidget()
        AgendaWidget()
        UnreadAccessory()
        LatestAccessory()
    }
}

// MARK: - Timeline

struct SnapshotEntry: TimelineEntry {
    let date: Date
    let snapshot: WidgetSnapshot?
    /// A gallery preview or placeholder: sample content, never the account's.
    var sample = false
}

struct SnapshotProvider: TimelineProvider {
    /// Agenda widgets redraw at each event's start and end; the rest only when the app says so.
    var followsAgenda = false

    func placeholder(in context: Context) -> SnapshotEntry {
        SnapshotEntry(date: Date(), snapshot: WidgetFixtures.snapshot(), sample: true)
    }

    func getSnapshot(in context: Context, completion: @escaping (SnapshotEntry) -> Void) {
        if context.isPreview {
            completion(SnapshotEntry(date: Date(), snapshot: WidgetFixtures.snapshot(), sample: true))
        } else {
            completion(SnapshotEntry(date: Date(), snapshot: WidgetStore.read()))
        }
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<SnapshotEntry>) -> Void) {
        let now = Date()
        let snapshot = WidgetStore.read()
        var entries = [SnapshotEntry(date: now, snapshot: snapshot)]
        if followsAgenda, let agenda = snapshot?.agenda {
            var moment = now
            while entries.count < 12, let next = agenda.nextChange(after: moment) {
                entries.append(SnapshotEntry(date: next, snapshot: snapshot))
                moment = next
            }
        }
        // Past the last event boundary, check again at the next midnight so "Today" stays today.
        let midnight = Calendar.current.nextDate(after: now, matching: DateComponents(hour: 0, minute: 1), matchingPolicy: .nextTime)
        completion(Timeline(entries: entries, policy: followsAgenda ? .after(midnight ?? now.addingTimeInterval(3600)) : .never))
    }
}

/// The kit's surface behind every system widget, and the type registered once.
private struct WidgetChrome: ViewModifier {
    @Environment(\.widgetRenderingMode) private var mode
    func body(content: Content) -> some View {
        content
            .containerBackground(for: .widget) {
                if mode == .fullColor { PP.Colors.surface } else { Color.clear }
            }
            .onAppear { PPFonts.register() }
    }
}

private extension View {
    func widgetChrome() -> some View { modifier(WidgetChrome()) }
}

// MARK: - Widgets

struct InboxWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: WidgetStore.Kind.inbox, provider: SnapshotProvider()) { entry in
            InboxEntryView(entry: entry).widgetChrome()
        }
        .configurationDisplayName("Inbox")
        .description("Unread conversations and who wrote last. Message text stays hidden unless you turn on previews in Settings.")
        .supportedFamilies([.systemSmall, .systemMedium, .systemLarge, .systemExtraLarge])
    }
}

private struct InboxEntryView: View {
    @Environment(\.widgetFamily) private var family
    let entry: SnapshotEntry
    var body: some View {
        InboxWidgetView(snapshot: entry.snapshot, face: WidgetFace(family), now: entry.date)
    }
}

struct QuickActionsWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: WidgetStore.Kind.quickActions, provider: SnapshotProvider()) { entry in
            QuickActionsWidgetView(snapshot: entry.snapshot).widgetChrome()
        }
        .configurationDisplayName("Quick actions")
        .description("Start a message, open your inbox or search it.")
        .supportedFamilies([.systemSmall])
    }
}

struct NestsWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: WidgetStore.Kind.nests, provider: SnapshotProvider()) { entry in
            NestsEntryView(entry: entry).widgetChrome()
        }
        .configurationDisplayName("Nests")
        .description("Your Nests, what they are doing, and their running hours.")
        .supportedFamilies([.systemSmall, .systemMedium])
    }
}

private struct NestsEntryView: View {
    @Environment(\.widgetFamily) private var family
    let entry: SnapshotEntry
    var body: some View { NestsWidgetView(snapshot: entry.snapshot, face: WidgetFace(family)) }
}

struct AgendaWidget: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: WidgetStore.Kind.agenda, provider: SnapshotProvider(followsAgenda: true)) { entry in
            AgendaEntryView(entry: entry).widgetChrome()
        }
        .configurationDisplayName("Agenda")
        .description("What is left today and what comes next on your Pigeonpost calendar.")
        .supportedFamilies([.systemMedium, .systemLarge])
    }
}

private struct AgendaEntryView: View {
    @Environment(\.widgetFamily) private var family
    let entry: SnapshotEntry
    var body: some View { AgendaWidgetView(snapshot: entry.snapshot, face: WidgetFace(family), now: entry.date) }
}

struct UnreadAccessory: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: WidgetStore.Kind.unread, provider: SnapshotProvider()) { entry in
            InboxWidgetView(snapshot: entry.snapshot, face: .circular, now: entry.date)
                .containerBackground(for: .widget) { Color.clear }
        }
        .configurationDisplayName("Unread")
        .description("How many conversations are waiting.")
        .supportedFamilies([.accessoryCircular])
    }
}

struct LatestAccessory: Widget {
    var body: some WidgetConfiguration {
        StaticConfiguration(kind: WidgetStore.Kind.latest, provider: SnapshotProvider()) { entry in
            InboxWidgetView(snapshot: entry.snapshot, face: .rectangular, now: entry.date)
                .containerBackground(for: .widget) { Color.clear }
        }
        .configurationDisplayName("Latest conversation")
        .description("Who wrote last. Message text stays hidden unless you turn on previews.")
        .supportedFamilies([.accessoryRectangular])
    }
}
