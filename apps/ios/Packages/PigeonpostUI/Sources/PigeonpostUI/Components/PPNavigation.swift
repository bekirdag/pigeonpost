//  Navigation: tab bar helpers (`.pp-tabbar`). Prefer the system TabView with `.ppTabBarStyle()`;
//  PPTabBar is a custom bar for screens that draw their own (macOS sidebars excluded).

import SwiftUI

public struct PPTabItem: Identifiable, Hashable, Sendable {
    public let id: String
    public let title: String
    public let icon: PPIconName
    public let badge: Int?

    public init(id: String, title: String, icon: PPIconName, badge: Int? = nil) {
        self.id = id
        self.title = title
        self.icon = icon
        self.badge = badge
    }
}

/// A Pigeonpost tab label for a system `TabView`: kit icon + Geist caption.
public struct PPTabLabel: View {
    let item: PPTabItem
    public init(_ item: PPTabItem) { self.item = item }
    public var body: some View {
        Label { Text(item.title) } icon: { PPIcon(item.icon, size: 24, weight: 1.7) }
    }
}

public extension View {
    /// Tints a system TabView with the kit's accent and surface.
    func ppTabBarStyle() -> some View {
        self.tint(PP.Colors.accent)
            #if os(iOS)
            .toolbarBackground(PP.Colors.surface.opacity(0.92), for: .tabBar)
            .toolbarBackground(.visible, for: .tabBar)
            #endif
    }
}

/// A custom iOS-style tab bar: 27pt icons (2.2 stroke when selected), 10.5pt labels, red count
/// badges, optional floating capsule (`floating`).
public struct PPTabBar: View {
    let items: [PPTabItem]
    @Binding var selection: String
    let floating: Bool

    public init(items: [PPTabItem], selection: Binding<String>, floating: Bool = false) {
        self.items = items
        self._selection = selection
        self.floating = floating
    }

    public var body: some View {
        HStack(spacing: 0) {
            ForEach(items) { item in
                let selected = item.id == selection
                Button { selection = item.id } label: {
                    VStack(spacing: 2) {
                        PPIcon(item.icon, size: 27, weight: selected ? 2.2 : 1.7)
                        Text(item.title).font(.geist(10.5, .medium))
                    }
                    .foregroundStyle(selected ? PP.Colors.accent : PP.Colors.textFaint)
                    .frame(maxWidth: .infinity, minHeight: 44)
                    .padding(.vertical, floating ? 5 : 2)
                    .background {
                        if floating && selected { Capsule().fill(PP.Colors.text.opacity(0.08)) }
                    }
                    .overlay(alignment: .top) {
                        if let badge = item.badge {
                            Text(badge > 0 ? "\(badge)" : "")
                                .font(.geist(12, .medium)).monospacedDigit()
                                .foregroundStyle(.white)
                                .padding(.horizontal, badge > 0 ? 5 : 0)
                                .frame(minWidth: badge > 0 ? 18 : 10, minHeight: badge > 0 ? 18 : 10)
                                .background(Capsule().fill(PP.Colors.dangerSolid))
                                .offset(x: 14, y: badge > 0 ? 0 : 2)
                        }
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityLabel(Text(item.title))
                .accessibilityValue(item.badge.map { $0 > 0 ? Text("\($0) new") : Text("New activity") } ?? Text(""))
                .accessibilityAddTraits(selected ? [.isSelected, .isButton] : .isButton)
            }
        }
        .padding(.horizontal, floating ? 6 : 8)
        .padding(.top, 6)
        .padding(.bottom, floating ? 6 : 2)
        .background {
            if floating {
                Capsule().fill(.regularMaterial).overlay(Capsule().strokeBorder(PP.Colors.border, lineWidth: 0.5)).ppShadow(.three)
            } else {
                Rectangle().fill(.regularMaterial).overlay(alignment: .top) { Rectangle().fill(PP.Colors.border).frame(height: 0.5) }
                    .ignoresSafeArea(edges: .bottom)
            }
        }
        .padding(.horizontal, floating ? 20 : 0)
        .accessibilityElement(children: .contain)
        .accessibilityAddTraits(.isTabBar)
    }
}

public extension PPTabItem {
    /// The Pigeonpost app's tabs.
    static let inbox = PPTabItem(id: "inbox", title: "Inbox", icon: .inbox)
    static let calls = PPTabItem(id: "calls", title: "Calls", icon: .phone)
    static let agents = PPTabItem(id: "agents", title: "Agents", icon: .bot)
    static let contacts = PPTabItem(id: "contacts", title: "Contacts", icon: .contact)
    static let settings = PPTabItem(id: "settings", title: "Settings", icon: .settings)
}

#Preview("Tab bars") {
    struct Demo: View {
        @State var tab = "inbox"
        var body: some View {
            VStack {
                Spacer()
                PPTabBar(items: [PPTabItem(id: "inbox", title: "Inbox", icon: .inbox, badge: 4), .calls, PPTabItem(id: "agents", title: "Agents", icon: .bot, badge: 0), .settings],
                         selection: $tab, floating: true)
                    .padding(.bottom, 24)
                PPTabBar(items: [.inbox, .calls, .agents, .contacts, .settings], selection: $tab)
            }
            .background(PP.Colors.bg)
        }
    }
    return Demo().pigeonpostTheme()
}
