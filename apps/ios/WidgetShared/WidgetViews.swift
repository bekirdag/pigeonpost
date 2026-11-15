//  The widgets' faces, drawn from a `WidgetSnapshot` alone.
//
//  Built on the design kit (`PigeonpostUI`): its colour roles follow light and dark, its type is
//  Geist, and the layout follows the kit's widget anatomy (mobile/widgets.html — `.pp-widget`:
//  a 42pt number, a muted label, 32pt avatars in rows, one accent line). In the tinted and clear
//  Home Screen styles (`widgetRenderingMode` .accented / .vibrant) the system recolours everything;
//  what should carry the tint — the unread number, the dots, the meter — is `widgetAccentable`.
//
//  Shared with the app, which renders the same views in its debug widget gallery, so a screenshot
//  of the gallery is a screenshot of these.

import SwiftUI
import WidgetKit
import PigeonpostUI

// MARK: - Sizes

/// Which face to draw. Mirrors `WidgetFamily` so the app's gallery can ask for one without WidgetKit
/// deciding the size.
enum WidgetFace: String, CaseIterable, Sendable {
    case small, medium, large, extraLarge, circular, rectangular

    init(_ family: WidgetFamily) {
        switch family {
        case .systemSmall: self = .small
        case .systemMedium: self = .medium
        case .systemLarge: self = .large
        case .systemExtraLarge: self = .extraLarge
        case .accessoryCircular: self = .circular
        case .accessoryRectangular: self = .rectangular
        default: self = .small
        }
    }

    /// How many conversation rows fit.
    var conversationRows: Int {
        switch self {
        case .small, .circular, .rectangular: return 1
        case .medium: return 2
        case .large: return 5
        case .extraLarge: return 8
        }
    }
}

// MARK: - Shared parts

enum WidgetTime {
    /// "9:38" today, "Yesterday", a weekday within the week, a short date beyond.
    static func label(_ date: Date, now: Date, calendar: Calendar = .current) -> String {
        if calendar.isDate(date, inSameDayAs: now) { return date.formatted(date: .omitted, time: .shortened) }
        if let yesterday = calendar.date(byAdding: .day, value: -1, to: now), calendar.isDate(date, inSameDayAs: yesterday) {
            return "Yesterday"
        }
        if let tomorrow = calendar.date(byAdding: .day, value: 1, to: now), calendar.isDate(date, inSameDayAs: tomorrow) {
            return "Tomorrow"
        }
        if let week = calendar.date(byAdding: .day, value: -6, to: now), date > week, date < now {
            return date.formatted(.dateTime.weekday(.abbreviated))
        }
        return date.formatted(.dateTime.day().month(.abbreviated))
    }

    static func eventTime(_ event: WidgetEvent) -> String {
        event.allDay ? "All day" : event.startsAt.formatted(date: .omitted, time: .shortened)
    }
}

/// The widget header: the bird mark, a title, and an optional trailing note.
struct WidgetHeader: View {
    let title: String
    var icon: PPIconName = .bird
    var trailing: String? = nil

    var body: some View {
        HStack(spacing: 6) {
            PPIcon(icon, size: 15, weight: 2.2)
                .foregroundStyle(PP.Colors.textLink)
                .widgetAccentable()
            Text(title)
                .font(.geist(13, .semibold))
                .foregroundStyle(PP.Colors.text)
                .lineLimit(1)
            Spacer(minLength: 4)
            if let trailing {
                Text(trailing)
                    .font(.geist(12, .medium))
                    .foregroundStyle(PP.Colors.textMuted)
                    .lineLimit(1)
            }
        }
        .accessibilityElement(children: .combine)
    }
}

/// The unread dot: `--pp-accent`, 8pt.
struct UnreadDot: View {
    var body: some View {
        Circle().fill(PP.Colors.accent).frame(width: 8, height: 8).widgetAccentable()
            .accessibilityHidden(true)
    }
}

/// A centred line or two for a widget with nothing to show yet.
struct WidgetMessage: View {
    let title: String
    let message: String
    var icon: PPIconName = .bird
    var linkTitle: String? = nil
    var link: URL? = nil

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            PPIcon(icon, size: 22).foregroundStyle(PP.Colors.textLink).widgetAccentable()
            Spacer(minLength: 0)
            Text(title).font(.geist(15, .semibold)).foregroundStyle(PP.Colors.text).lineLimit(2)
            Text(message).font(.geist(12.5)).foregroundStyle(PP.Colors.textMuted).lineLimit(3)
            if let linkTitle, let link {
                Link(destination: link) {
                    Text(linkTitle).font(.geist(13, .semibold)).foregroundStyle(PP.Colors.textLink).widgetAccentable()
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
    }
}

// MARK: - Inbox

struct InboxWidgetView: View {
    let snapshot: WidgetSnapshot?
    let face: WidgetFace
    var now: Date = Date()

    var body: some View {
        Group {
            if let snapshot {
                switch face {
                case .small: small(snapshot)
                case .circular: circular(snapshot)
                case .rectangular: rectangular(snapshot)
                default: list(snapshot)
                }
            } else {
                empty
            }
        }
        .widgetURL(WidgetRoute.inbox.url)
    }

    @ViewBuilder private var empty: some View {
        switch face {
        case .circular:
            ZStack {
                AccessoryWidgetBackground()
                PPIcon(.bird, size: 20)
            }
            .accessibilityLabel("Pigeonpost")
        case .rectangular:
            VStack(alignment: .leading, spacing: 2) {
                Text("Pigeonpost").font(.geist(14, .semibold))
                Text("Open the app to see your inbox").font(.geist(12)).lineLimit(2)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        default:
            WidgetMessage(title: "Pigeonpost", message: "Open Pigeonpost to see your inbox here.")
        }
    }

    private func small(_ snapshot: WidgetSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            WidgetHeader(title: "Inbox")
            Spacer(minLength: 4)
            Text("\(snapshot.unreadCount)")
                .font(.geist(42, .bold))
                .monospacedDigit()
                .tracking(-0.84)
                .foregroundStyle(PP.Colors.text)
                .widgetAccentable()
                .minimumScaleFactor(0.5)
                .lineLimit(1)
            Text(snapshot.unreadCount == 1 ? "unread conversation" : "unread")
                .font(.geist(13))
                .foregroundStyle(PP.Colors.textMuted)
            Spacer(minLength: 4)
            if let latest = snapshot.conversations.first {
                HStack(spacing: 4) {
                    if latest.unread > 0 { UnreadDot() }
                    Text(latest.name).font(.geist(13, .medium)).foregroundStyle(PP.Colors.textBody).lineLimit(1)
                    Spacer(minLength: 2)
                    Text(WidgetTime.label(latest.lastAt, now: now)).font(.geist(12)).foregroundStyle(PP.Colors.textFaint)
                }
            } else {
                Text("No conversations yet").font(.geist(13)).foregroundStyle(PP.Colors.textMuted).lineLimit(1)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .leading)
        .accessibilityElement(children: .combine)
        .accessibilityLabel(Self.summary(snapshot))
        .accessibilityIdentifier("widget-inbox-small")
    }

    private func list(_ snapshot: WidgetSnapshot) -> some View {
        let rows = Array(snapshot.conversations.prefix(face.conversationRows))
        let columns = face == .extraLarge ? 2 : 1
        return VStack(alignment: .leading, spacing: face == .medium ? 8 : 10) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                WidgetHeader(title: "Inbox", trailing: snapshot.mailbox.title)
            }
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Text("\(snapshot.unreadCount)")
                    .font(.geist(face == .medium ? 22 : 28, .bold))
                    .monospacedDigit()
                    .foregroundStyle(PP.Colors.text)
                    .widgetAccentable()
                Text("unread").font(.geist(13)).foregroundStyle(PP.Colors.textMuted)
                Spacer(minLength: 0)
            }
            .accessibilityElement(children: .combine)
            if rows.isEmpty {
                Text("No conversations yet").font(.geist(13)).foregroundStyle(PP.Colors.textMuted)
                Spacer(minLength: 0)
            } else if columns == 2 {
                let half = (rows.count + 1) / 2
                HStack(alignment: .top, spacing: 18) {
                    column(Array(rows.prefix(half)), snapshot: snapshot)
                    column(Array(rows.dropFirst(half)), snapshot: snapshot)
                }
                Spacer(minLength: 0)
            } else {
                column(rows, snapshot: snapshot)
                Spacer(minLength: 0)
            }
            if face == .large || face == .extraLarge {
                Text("Updated \(snapshot.generatedAt.formatted(date: .omitted, time: .shortened))")
                    .font(.geist(11.5))
                    .foregroundStyle(PP.Colors.textFaint)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .accessibilityIdentifier("widget-inbox-\(face.rawValue)")
    }

    private func column(_ rows: [WidgetConversation], snapshot: WidgetSnapshot) -> some View {
        VStack(alignment: .leading, spacing: 8) {
            ForEach(rows) { row in
                Link(destination: WidgetRoute.conversation(mailbox: snapshot.mailbox.address, peer: row.peer).url) {
                    WidgetConversationRow(row: row, now: now)
                }
            }
        }
        .frame(maxWidth: .infinity, alignment: .topLeading)
    }

    private func circular(_ snapshot: WidgetSnapshot) -> some View {
        ZStack {
            AccessoryWidgetBackground()
            VStack(spacing: 0) {
                PPIcon(.bird, size: 12, weight: 2.4)
                Text(snapshot.unreadCount > 99 ? "99+" : "\(snapshot.unreadCount)")
                    .font(.geist(20, .bold))
                    .monospacedDigit()
                    .minimumScaleFactor(0.5)
                    .widgetAccentable()
            }
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Self.summary(snapshot))
        .accessibilityIdentifier("widget-unread-circular")
    }

    private func rectangular(_ snapshot: WidgetSnapshot) -> some View {
        let latest = snapshot.conversations.first
        return VStack(alignment: .leading, spacing: 1) {
            HStack(spacing: 4) {
                PPIcon(.bird, size: 11, weight: 2.4)
                Text(snapshot.unreadCount == 0 ? "Pigeonpost" : "\(snapshot.unreadCount) unread")
                    .font(.geist(12, .semibold))
                    .widgetAccentable()
            }
            if let latest {
                Text(latest.name).font(.geist(14, .semibold)).lineLimit(1)
                Text(WidgetConversationRow.subtitle(latest))
                    .font(.geist(12))
                    .lineLimit(1)
                    .privacySensitive(latest.preview != nil)
            } else {
                Text("No conversations yet").font(.geist(13)).lineLimit(1)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .widgetURL(latest.map { WidgetRoute.conversation(mailbox: snapshot.mailbox.address, peer: $0.peer).url } ?? WidgetRoute.inbox.url)
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("widget-latest-rectangular")
    }

    static func summary(_ snapshot: WidgetSnapshot) -> String {
        let count = snapshot.unreadCount
        let head = count == 0 ? "Pigeonpost, no unread conversations" : "Pigeonpost, \(count) unread"
        guard let latest = snapshot.conversations.first else { return head }
        return head + ". Latest: " + latest.name
    }
}

/// One conversation: avatar initials, name, a line under it, time and the unread dot.
struct WidgetConversationRow: View {
    let row: WidgetConversation
    let now: Date

    /// The preview when previews are on, otherwise "New message" for unread mail and the address
    /// for the rest. Never message text unless the snapshot carries it.
    static func subtitle(_ row: WidgetConversation) -> String {
        if let preview = row.preview { return preview }
        if row.unread > 1 { return "\(row.unread) new messages" }
        if row.unread == 1 { return "New message" }
        return row.peer == row.name ? "No new messages" : row.peer
    }

    var body: some View {
        HStack(spacing: 10) {
            if row.kind == .agent {
                PPAvatar(initials: "", tone: .green, size: 32, icon: .bot)
            } else {
                PPAvatar(peer: row.peer, name: row.name, size: 32)
            }
            VStack(alignment: .leading, spacing: 1) {
                Text(row.name)
                    .font(.geist(14, row.unread > 0 ? .semibold : .medium))
                    .foregroundStyle(PP.Colors.text)
                    .lineLimit(1)
                Text(Self.subtitle(row))
                    .font(row.preview == nil && row.unread == 0 ? .geistMono(11.5) : .geist(12.5))
                    .foregroundStyle(row.unread > 0 && row.preview == nil ? PP.Colors.textLink : PP.Colors.textMuted)
                    .lineLimit(1)
                    .privacySensitive(row.preview != nil)
            }
            Spacer(minLength: 4)
            VStack(alignment: .trailing, spacing: 4) {
                Text(WidgetTime.label(row.lastAt, now: now))
                    .font(.geist(12))
                    .foregroundStyle(PP.Colors.textFaint)
                    .lineLimit(1)
                if row.unread > 0 { UnreadDot() } else { Color.clear.frame(width: 8, height: 8) }
            }
        }
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("widget-row-\(row.peer)")
    }
}

// MARK: - Quick actions

struct QuickActionsWidgetView: View {
    let snapshot: WidgetSnapshot?

    private struct Action: Identifiable {
        let title: String
        let icon: PPIconName
        let route: WidgetRoute
        var id: String { title }
    }

    private var actions: [Action] {
        [Action(title: "New message", icon: .edit, route: .compose),
         Action(title: "Inbox", icon: .inbox, route: .inbox),
         Action(title: "Search", icon: .search, route: .search)]
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            WidgetHeader(title: "Pigeonpost", trailing: snapshot.flatMap { $0.unreadCount > 0 ? "\($0.unreadCount)" : nil })
            Spacer(minLength: 2)
            ForEach(actions) { action in
                Link(destination: action.route.url) {
                    HStack(spacing: 8) {
                        PPIcon(action.icon, size: 15)
                            .foregroundStyle(PP.Colors.textLink)
                            .widgetAccentable()
                        Text(action.title)
                            .font(.geist(13.5, .medium))
                            .foregroundStyle(PP.Colors.text)
                            .lineLimit(1)
                            .minimumScaleFactor(0.8)
                        Spacer(minLength: 0)
                    }
                    .padding(.horizontal, 10)
                    .frame(height: 32)
                    .background(RoundedRectangle(cornerRadius: 16, style: .continuous).fill(PP.Colors.accentSubtle))
                }
                .accessibilityIdentifier("widget-action-\(action.title)")
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .widgetURL(WidgetRoute.inbox.url)
        .accessibilityIdentifier("widget-quick-actions")
    }
}

// MARK: - Nests

struct NestsWidgetView: View {
    let snapshot: WidgetSnapshot?
    let face: WidgetFace

    var body: some View {
        Group {
            if let nests = snapshot?.nests {
                if nests.items.isEmpty {
                    if nests.available {
                        WidgetMessage(title: "No Nests yet", message: "A Nest is a hosted agent with its own workspace.",
                                      icon: .egg, linkTitle: "Get a Nest", link: WidgetRoute.getNest.url)
                    } else {
                        WidgetMessage(title: "Nests", message: "Nests are not available for this account yet.", icon: .egg)
                    }
                } else {
                    content(nests.items)
                }
            } else {
                WidgetMessage(title: "Nests", message: "Open Pigeonpost to see your Nests here.", icon: .egg)
            }
        }
        .widgetURL(WidgetRoute.nests.url)
        .accessibilityIdentifier("widget-nests-\(face.rawValue)")
    }

    private func content(_ items: [WidgetNest]) -> some View {
        let shown = Array(items.prefix(face == .small ? 1 : 2))
        return VStack(alignment: .leading, spacing: face == .small ? 8 : 9) {
            WidgetHeader(title: "Nests", icon: .egg, trailing: items.count > 1 ? "\(items.count)" : nil)
            if face == .small, let nest = shown.first {
                Spacer(minLength: 0)
                Text(nest.name).font(.geist(16, .semibold)).foregroundStyle(PP.Colors.text).lineLimit(2)
                NestPill(nest: nest)
                Spacer(minLength: 0)
                hours(nest)
            } else {
                ForEach(shown) { nest in
                    VStack(alignment: .leading, spacing: 4) {
                        HStack(spacing: 8) {
                            Text(nest.name).font(.geist(14, .semibold)).foregroundStyle(PP.Colors.text).lineLimit(1)
                            Spacer(minLength: 4)
                            NestPill(nest: nest)
                        }
                        compactHours(nest)
                    }
                    .accessibilityElement(children: .combine)
                }
                Spacer(minLength: 0)
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    @ViewBuilder private func hours(_ nest: WidgetNest) -> some View {
        if let fraction = nest.meterFraction, let used = nest.runningHours, let limit = nest.hoursLimit {
            VStack(alignment: .leading, spacing: 4) {
                HStack {
                    Text("Running").foregroundStyle(PP.Colors.textMuted)
                    Spacer(minLength: 4)
                    Text("\(Self.hoursText(used)) of \(limit) h").fontWeight(.medium).monospacedDigit().foregroundStyle(PP.Colors.text)
                }
                .font(.geist(11.5))
                GeometryReader { proxy in
                    ZStack(alignment: .leading) {
                        Capsule().fill(PP.Colors.surfaceSunken)
                        Capsule().fill(fraction >= 1 ? PP.Colors.dangerSolid : (fraction >= 0.8 ? PP.Colors.warningSolid : PP.Colors.accent))
                            .frame(width: max(4, proxy.size.width * fraction))
                            .widgetAccentable()
                    }
                }
                .frame(height: 6)
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel("Running time, \(Self.hoursText(used)) of \(limit) hours")
        } else if let limit = nest.hoursLimit {
            Text("\(limit) running hours a month").font(.geist(11.5)).foregroundStyle(PP.Colors.textMuted).lineLimit(1)
        }
    }

    /// The meter on one line, for rows: bar, then "42 of 120 h".
    @ViewBuilder private func compactHours(_ nest: WidgetNest) -> some View {
        if let fraction = nest.meterFraction, let used = nest.runningHours, let limit = nest.hoursLimit {
            HStack(spacing: 8) {
                bar(fraction)
                Text("\(Self.hoursText(used)) of \(limit) h").font(.geist(11.5, .medium)).monospacedDigit()
                    .foregroundStyle(PP.Colors.textMuted).fixedSize()
            }
            .accessibilityElement(children: .combine)
            .accessibilityLabel("Running time, \(Self.hoursText(used)) of \(limit) hours")
        } else if let limit = nest.hoursLimit {
            Text("\(limit) running hours a month").font(.geist(11.5)).foregroundStyle(PP.Colors.textMuted).lineLimit(1)
        }
    }

    private func bar(_ fraction: Double) -> some View {
        GeometryReader { proxy in
            ZStack(alignment: .leading) {
                Capsule().fill(PP.Colors.surfaceSunken)
                Capsule().fill(fraction >= 1 ? PP.Colors.dangerSolid : (fraction >= 0.8 ? PP.Colors.warningSolid : PP.Colors.accent))
                    .frame(width: max(4, proxy.size.width * fraction))
                    .widgetAccentable()
            }
        }
        .frame(height: 6)
    }

    static func hoursText(_ hours: Double) -> String {
        hours < 10 ? String(format: "%.1f", hours) : String(Int(hours.rounded()))
    }
}

/// The kit's `.pp-nest-status` pill, coloured by the Nest's state, with the app's own words.
struct NestPill: View {
    let nest: WidgetNest

    var body: some View {
        let (dot, ink, fill) = colors
        HStack(spacing: 5) {
            Circle().fill(dot).frame(width: 7, height: 7).widgetAccentable()
            Text(nest.status).lineLimit(1)
        }
        .font(.geist(11.5, .medium))
        .foregroundStyle(ink)
        .padding(.horizontal, 8)
        .frame(height: 22)
        .background(Capsule().fill(fill))
        .accessibilityElement(children: .combine)
    }

    private var colors: (Color, Color, Color) {
        switch nest.tone {
        case .ready: return (PP.Colors.successSolid, PP.Colors.success, PP.Colors.successFill)
        case .working, .setup: return (PP.Colors.infoSolid, PP.Colors.info, PP.Colors.infoFill)
        case .attention, .paused: return (PP.Colors.warningSolid, PP.Colors.warning, PP.Colors.warningFill)
        case .failed: return (PP.Colors.dangerSolid, PP.Colors.danger, PP.Colors.dangerFill)
        case .unknown: return (PP.Colors.textFaint, PP.Colors.textMuted, PP.Colors.surfaceSunken)
        }
    }
}

// MARK: - Agenda

struct AgendaWidgetView: View {
    let snapshot: WidgetSnapshot?
    let face: WidgetFace
    var now: Date = Date()

    var body: some View {
        Group {
            if let snapshot, let agenda = snapshot.agenda {
                content(agenda, mailbox: snapshot.mailbox.address)
            } else {
                WidgetMessage(title: "Calendar", message: "Open Pigeonpost to see today's events here.", icon: .calendar)
            }
        }
        .widgetURL(WidgetRoute.calendar.url)
        .accessibilityIdentifier("widget-agenda-\(face.rawValue)")
    }

    private func content(_ agenda: WidgetAgenda, mailbox: String) -> some View {
        let (today, next) = agenda.todayAndNext(at: now)
        let todayRows = Array(today.prefix(face == .large ? 5 : 2))
        return VStack(alignment: .leading, spacing: 8) {
            WidgetHeader(title: "Today", icon: .calendar, trailing: now.formatted(.dateTime.weekday(.abbreviated).day().month(.abbreviated)))
            if todayRows.isEmpty {
                Text("Nothing else today").font(.geist(13)).foregroundStyle(PP.Colors.textMuted)
            } else {
                ForEach(todayRows) { event in row(event, mailbox: mailbox, showDay: false) }
            }
            if today.count > todayRows.count {
                Text("+\(today.count - todayRows.count) more today").font(.geist(12)).foregroundStyle(PP.Colors.textMuted)
            }
            if face == .large || todayRows.count < 2 {
                Text("NEXT").font(.geist(10.5, .semibold)).tracking(0.6).foregroundStyle(PP.Colors.textFaint).padding(.top, 2)
                if let next {
                    row(next, mailbox: mailbox, showDay: true)
                } else {
                    Text("No upcoming events").font(.geist(13)).foregroundStyle(PP.Colors.textMuted)
                }
            }
            Spacer(minLength: 0)
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
    }

    private func row(_ event: WidgetEvent, mailbox: String, showDay: Bool) -> some View {
        Link(destination: WidgetRoute.event(mailbox: mailbox, eventId: event.eventId, occurrenceId: event.occurrenceId).url) {
            HStack(alignment: .top, spacing: 8) {
                RoundedRectangle(cornerRadius: 2).fill(PP.Colors.accent).frame(width: 3).widgetAccentable()
                VStack(alignment: .leading, spacing: 1) {
                    HStack(spacing: 4) {
                        Text(showDay && !Calendar.current.isDate(event.startsAt, inSameDayAs: now)
                             ? WidgetTime.label(event.startsAt, now: now) + " · " + WidgetTime.eventTime(event)
                             : WidgetTime.eventTime(event))
                            .font(.geistMono(11.5, .medium))
                            .foregroundStyle(PP.Colors.textMuted)
                        if event.hasMeeting { PPIcon(.video, size: 11).foregroundStyle(PP.Colors.textLink) }
                    }
                    Text(event.title).font(.geist(13.5, .semibold)).foregroundStyle(PP.Colors.text).lineLimit(1)
                    if face == .large, let location = event.location {
                        Text(location).font(.geist(12)).foregroundStyle(PP.Colors.textMuted).lineLimit(1)
                    }
                }
                Spacer(minLength: 0)
            }
            .fixedSize(horizontal: false, vertical: true)
        }
        .accessibilityElement(children: .combine)
    }
}
