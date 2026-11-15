//  Debug builds only: the widgets' own views at their Home Screen sizes, with the fixture snapshot.
//
//  `-widget-gallery` replaces the app's root with this, for the UI check (UITests WidgetGalleryTests)
//  and for the screenshots in the pull request. `-widget-page=1…3` picks a page, `-widget-dark`
//  forces dark, `-widget-previews` shows the fixture with message previews switched on.
//  These are the same SwiftUI views the extension draws (WidgetShared/WidgetViews.swift).

#if DEBUG
import SwiftUI
import PigeonpostUI

struct WidgetGallery: View {
    static var enabled: Bool { CommandLine.arguments.contains("-widget-gallery") }
    private static func argument(_ name: String) -> String? {
        CommandLine.arguments.first { $0.hasPrefix("-\(name)=") }.map { String($0.dropFirst(name.count + 2)) }
    }

    private let page = Int(WidgetGallery.argument("widget-page") ?? "1") ?? 1
    private let dark = CommandLine.arguments.contains("-widget-dark")
    private let previews = CommandLine.arguments.contains("-widget-previews")
    @Environment(\.horizontalSizeClass) private var sizeClass

    /// Home Screen sizes in points: iPhone 16 Pro, and a 13-inch iPad.
    private var pad: Bool { sizeClass == .regular }
    private var small: CGSize { pad ? CGSize(width: 170, height: 170) : CGSize(width: 170, height: 170) }
    private var medium: CGSize { pad ? CGSize(width: 379, height: 170) : CGSize(width: 364, height: 170) }
    private var large: CGSize { pad ? CGSize(width: 379, height: 379) : CGSize(width: 364, height: 382) }
    private var extraLarge: CGSize { CGSize(width: 795, height: 379) }

    private var now: Date {
        Calendar.current.date(bySettingHour: 9, minute: 41, second: 0, of: Date()) ?? Date()
    }
    private var snapshot: WidgetSnapshot { WidgetFixtures.snapshot(now: now, previews: previews) }

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 18) {
                Text(title).font(.geist(13, .semibold)).foregroundStyle(.white.opacity(0.85))
                    .accessibilityIdentifier("widget-gallery")
                if pad { padPage } else { phonePage }
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 20)
            .frame(maxWidth: .infinity)
        }
        .background(wallpaper.ignoresSafeArea())
        .preferredColorScheme(dark ? .dark : .light)
        .pigeonpostTheme(PPTheme(colorScheme: dark ? .dark : .light))
    }

    private var title: String {
        "Pigeonpost widgets · \(dark ? "dark" : "light")\(previews ? " · previews on" : "")"
    }

    @ViewBuilder private var phonePage: some View {
        switch page {
        case 2:
            HStack(spacing: 22) {
                tile(small) { QuickActionsWidgetView(snapshot: snapshot) }
                tile(small) { NestsWidgetView(snapshot: snapshot, face: .small) }
            }
            tile(medium) { NestsWidgetView(snapshot: snapshot, face: .medium) }
            tile(medium) { AgendaWidgetView(snapshot: snapshot, face: .medium, now: now) }
            HStack(spacing: 22) {
                tile(small) { NestsWidgetView(snapshot: WidgetFixtures.noNests(now: now), face: .small) }
                tile(small) { InboxWidgetView(snapshot: nil, face: .small) }
            }
        case 3:
            tile(large) { AgendaWidgetView(snapshot: snapshot, face: .large, now: now) }
            lockScreen
        default:
            HStack(spacing: 22) {
                tile(small) { InboxWidgetView(snapshot: snapshot, face: .small, now: now) }
                tile(small) { QuickActionsWidgetView(snapshot: snapshot) }
            }
            tile(medium) { InboxWidgetView(snapshot: snapshot, face: .medium, now: now) }
            tile(large) { InboxWidgetView(snapshot: snapshot, face: .large, now: now) }
        }
    }

    @ViewBuilder private var padPage: some View {
        switch page {
        case 2:
            HStack(alignment: .top, spacing: 30) {
                VStack(spacing: 30) {
                    HStack(spacing: 30) {
                        tile(small) { QuickActionsWidgetView(snapshot: snapshot) }
                        tile(small) { NestsWidgetView(snapshot: snapshot, face: .small) }
                    }
                    tile(medium) { NestsWidgetView(snapshot: snapshot, face: .medium) }
                    tile(medium) { AgendaWidgetView(snapshot: snapshot, face: .medium, now: now) }
                }
                VStack(spacing: 30) {
                    tile(large) { AgendaWidgetView(snapshot: snapshot, face: .large, now: now) }
                    lockScreen
                }
            }
        default:
            tile(extraLarge) { InboxWidgetView(snapshot: snapshot, face: .extraLarge, now: now) }
            HStack(alignment: .top, spacing: 30) {
                VStack(spacing: 30) {
                    HStack(spacing: 30) {
                        tile(small) { InboxWidgetView(snapshot: snapshot, face: .small, now: now) }
                        tile(small) { NestsWidgetView(snapshot: WidgetFixtures.noNests(now: now), face: .small) }
                    }
                    tile(medium) { InboxWidgetView(snapshot: snapshot, face: .medium, now: now) }
                }
                tile(large) { InboxWidgetView(snapshot: snapshot, face: .large, now: now) }
            }
        }
    }

    /// The Lock Screen accessories, white on the wallpaper as the system draws them.
    private var lockScreen: some View {
        HStack(spacing: 14) {
            InboxWidgetView(snapshot: snapshot, face: .circular, now: now)
                .frame(width: 72, height: 72)
            InboxWidgetView(snapshot: snapshot, face: .rectangular, now: now)
                .frame(width: 172, height: 72)
        }
        .foregroundStyle(.white)
        .environment(\.colorScheme, .dark)
        .padding(14)
        .background(RoundedRectangle(cornerRadius: 22).fill(.black.opacity(0.25)))
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("widget-lock-screen")
    }

    private func tile<Content: View>(_ size: CGSize, @ViewBuilder content: () -> Content) -> some View {
        content()
            .padding(16)
            .frame(width: size.width, height: size.height)
            .background(RoundedRectangle(cornerRadius: 22, style: .continuous).fill(PP.Colors.surface))
            .clipShape(RoundedRectangle(cornerRadius: 22, style: .continuous))
            .shadow(color: .black.opacity(0.18), radius: 10, y: 4)
            .accessibilityElement(children: .contain)
    }

    private var wallpaper: some View {
        LinearGradient(colors: dark ? [PP.Palette.navy900, .black] : [PP.Palette.blue500, PP.Palette.navy700],
                       startPoint: .topLeading, endPoint: .bottomTrailing)
    }
}
#endif
