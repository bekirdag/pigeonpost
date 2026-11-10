import SwiftUI

/// Week and day time grid with a now line (SDS §14B.13: "week time grid on iPad and Mac").
/// The iPhone keeps the month and agenda, which is what fits a phone.
struct CalendarTimeGrid: View {
    let store: CalendarStore
    let days: [CalendarDay]
    let open: (EventOccurrence) -> Void

    private let hourHeight: CGFloat = 52
    private let gutter: CGFloat = 52

    var body: some View {
        VStack(spacing: 0) {
            header
            allDayStrip
            Divider()
            ScrollViewReader { proxy in
                ScrollView(.vertical) {
                    TimelineView(.periodic(from: .now, by: 30)) { _ in
                        grid(now: store.currentTime())
                    }
                }
                .onAppear { scrollToStart(proxy) }
                .onChange(of: days) { _, _ in scrollToStart(proxy) }
                .accessibilityIdentifier("calendar-time-grid")
            }
        }
    }

    private func scrollToStart(_ proxy: ScrollViewProxy) {
        let blocks = days.flatMap { CalendarGrid.blocks(store.occurrences, day: $0, zone: store.zone) }
        let hour = CalendarGrid.initialHour(now: store.currentTime(), days: days, blocks: blocks, zone: store.zone)
        proxy.scrollTo("hour-\(hour)", anchor: .top)
    }

    private var header: some View {
        HStack(spacing: 0) {
            Color.clear.frame(width: gutter, height: 1)
            ForEach(days) { day in
                let today = day == CalendarDay(date: store.currentTime(), zone: store.zone)
                Button { Task { await store.select(day) } } label: {
                    VStack(spacing: 2) {
                        Text(CalendarPresentation.formatter("EEE", zone: CalendarDay.utc.timeZone).string(from: day.pickerDate))
                            .font(.geist(11.5, .semibold)).foregroundStyle(Theme.secondary)
                        Text("\(day.day)")
                            .font(.geist(16, today ? .bold : .medium))
                            .foregroundStyle(today ? Theme.onAccent : Theme.ink)
                            .frame(width: 30, height: 30)
                            .background(today ? Theme.accent : .clear, in: Circle())
                    }
                    .frame(maxWidth: .infinity).padding(.vertical, 6)
                    .background(day == store.selectedDay && days.count > 1 ? Theme.selected : .clear)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(CalendarPresentation.day(day) + (today ? ", today" : ""))
                .accessibilityAddTraits(day == store.selectedDay ? [.isSelected] : [])
                .accessibilityIdentifier("calendar-grid-day:" + day.id)
            }
        }
    }

    @ViewBuilder
    private var allDayStrip: some View {
        let rows = days.map { CalendarGrid.allDay(store.occurrences, day: $0, zone: store.zone) }
        if rows.contains(where: { !$0.isEmpty }) {
            HStack(alignment: .top, spacing: 0) {
                Text("All day").font(.geist(10.5)).foregroundStyle(Theme.secondary)
                    .frame(width: gutter, alignment: .trailing).padding(.trailing, 6)
                ForEach(Array(zip(days, rows)), id: \.0.id) { _, items in
                    VStack(spacing: 2) {
                        ForEach(items) { item in
                            Button { open(item) } label: {
                                Text(item.title ?? "Event").font(.geist(11.5, .medium)).lineLimit(1)
                                    .frame(maxWidth: .infinity, alignment: .leading).padding(.horizontal, 4).padding(.vertical, 2)
                                    .background(CalendarPresentation.color(item.color).opacity(0.22), in: RoundedRectangle(cornerRadius: 4))
                                    .foregroundStyle(Theme.ink)
                            }
                            .buttonStyle(.plain)
                            .accessibilityLabel((item.title ?? "Event") + ", all day")
                            .accessibilityIdentifier("calendar-grid-event:" + item.occurrenceId)
                        }
                    }
                    .frame(maxWidth: .infinity).padding(.horizontal, 2)
                }
            }
            .padding(.vertical, 4)
        }
    }

    private func grid(now: Date) -> some View {
        HStack(alignment: .top, spacing: 0) {
            // Hour labels; each is also the scroll anchor for its hour.
            VStack(spacing: 0) {
                ForEach(0..<24, id: \.self) { hour in
                    Text(hour == 0 ? "" : CalendarTimeGrid.hourLabel(hour))
                        .font(.geist(10.5)).foregroundStyle(Theme.secondary)
                        .frame(width: gutter - 6, height: hourHeight, alignment: .topTrailing)
                        .offset(y: -6)
                        .id("hour-\(hour)")
                }
            }
            .frame(width: gutter, alignment: .leading)
            .accessibilityHidden(true)
            ForEach(days) { day in
                column(day, now: now)
            }
        }
        .frame(height: hourHeight * 24)
    }

    private func column(_ day: CalendarDay, now: Date) -> some View {
        let blocks = CalendarGrid.blocks(store.occurrences, day: day, zone: store.zone)
        let items = Dictionary(store.occurrences.map { ($0.occurrenceId, $0) }, uniquingKeysWith: { first, _ in first })
        return GeometryReader { geometry in
            let width = geometry.size.width
            ZStack(alignment: .topLeading) {
                // Hour lines.
                ForEach(0..<24, id: \.self) { hour in
                    Rectangle().fill(Theme.rule).frame(height: 0.5).offset(y: CGFloat(hour) * hourHeight)
                }
                Rectangle().fill(Theme.rule).frame(width: 0.5).frame(maxHeight: .infinity)
                ForEach(blocks) { block in
                    if let item = items[block.occurrenceId] {
                        let slot = (width - 4) / CGFloat(block.columns)
                        Button { open(item) } label: { blockLabel(item, block) }
                            .buttonStyle(.plain)
                            .frame(width: max(slot - 2, 8),
                                   height: max(CGFloat(block.endMinute - block.startMinute) / 60 * hourHeight - 2, 14),
                                   alignment: .topLeading)
                            .offset(x: 2 + CGFloat(block.column) * slot, y: CGFloat(block.startMinute) / 60 * hourHeight + 1)
                            .accessibilityLabel(accessibility(item))
                            .accessibilityIdentifier("calendar-grid-event:" + item.occurrenceId)
                    }
                }
                // The now line, on today's column only.
                if let minute = CalendarGrid.nowMinute(now, day: day, zone: store.zone) {
                    let y = CGFloat(minute) / 60 * hourHeight
                    Rectangle().fill(Color.red).frame(width: width, height: 1.5).offset(y: y)
                    Circle().fill(Color.red).frame(width: 9, height: 9).offset(x: -4.5, y: y - 3.75)
                        .accessibilityElement()
                        .accessibilityLabel("Now")
                        .accessibilityIdentifier("calendar-now-line")
                }
            }
        }
        .frame(maxWidth: .infinity)
        .frame(height: hourHeight * 24)
    }

    private func blockLabel(_ item: EventOccurrence, _ block: CalendarGridBlock) -> some View {
        let color = CalendarPresentation.color(item.color)
        return VStack(alignment: .leading, spacing: 1) {
            Text(item.title ?? "Event").font(.geist(11.5, .semibold)).lineLimit(2)
                .strikethrough(item.status == "cancelled")
            if block.endMinute - block.startMinute >= 45 {
                Text(CalendarTimeGrid.minuteLabel(block.startMinute)).font(.geist(10.5)).foregroundStyle(Theme.secondary)
            }
        }
        .foregroundStyle(Theme.ink)
        .padding(.horizontal, 4).padding(.vertical, 2)
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        .background(color.opacity(item.invited == true && item.myRsvp == nil ? 0.08 : 0.2), in: RoundedRectangle(cornerRadius: 5))
        .overlay(alignment: .leading) { Rectangle().fill(color).frame(width: 3) }
        .overlay(RoundedRectangle(cornerRadius: 5).stroke(color.opacity(0.5), lineWidth: item.invited == true && item.myRsvp == nil ? 1 : 0))
        .clipShape(RoundedRectangle(cornerRadius: 5))
    }

    private func accessibility(_ item: EventOccurrence) -> String {
        var parts = [item.title ?? "Event", CalendarPresentation.time(item, zone: store.zone)]
        if item.status == "cancelled" { parts.append("cancelled") }
        else if item.invited == true { parts.append(CalendarPresentation.rsvp(item.myRsvp)) }
        if item.meeting != nil { parts.append("scheduled call") }
        return parts.joined(separator: ", ")
    }

    static func hourLabel(_ hour: Int) -> String {
        var parts = DateComponents(); parts.hour = hour; parts.minute = 0
        let date = CalendarDay.utc.date(from: DateComponents(year: 2000, month: 1, day: 1, hour: hour)) ?? Date()
        _ = parts
        return CalendarPresentation.formatter(DateFormatter.dateFormat(fromTemplate: "j", options: 0, locale: .current) ?? "HH",
                                              zone: CalendarDay.utc.timeZone).string(from: date)
    }

    static func minuteLabel(_ minute: Int) -> String {
        let date = CalendarDay.utc.date(from: DateComponents(year: 2000, month: 1, day: 1, hour: minute / 60, minute: minute % 60)) ?? Date()
        return CalendarPresentation.formatter(DateFormatter.dateFormat(fromTemplate: "jmm", options: 0, locale: .current) ?? "HH:mm",
                                              zone: CalendarDay.utc.timeZone).string(from: date)
    }
}
