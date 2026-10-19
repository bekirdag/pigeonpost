//  Light, dark, or whatever the system is doing — the person's choice, kept between launches.
//
//  Applied at the platform level, not through SwiftUI's `preferredColorScheme`: once that has pinned
//  a window to light or dark, setting it back to `nil` does not unpin it, so choosing System after
//  Light left a Mac window half light (seen: the toolbar followed, the content did not).
//  - iOS: every window's `overrideUserInterfaceStyle`, so sheets, popovers, the edit menu and the
//    UIKit-drawn message text all follow.
//  - macOS: `NSApp.appearance`, which every window, sheet, menu and AppKit text view inherits.
//  Every colour in `Theme` is resolved at draw time, so the whole app recolours in place.

import SwiftUI
#if canImport(UIKit)
import UIKit
#elseif canImport(AppKit)
import AppKit
#endif

enum AppearanceChoice: String, CaseIterable, Identifiable {
    case system, light, dark

    /// The `@AppStorage` key the choice is kept under.
    static let storageKey = "appearance"

    var id: String { rawValue }

    var label: String {
        switch self {
        case .system: return "System"
        case .light: return "Light"
        case .dark: return "Dark"
        }
    }

    /// Make the whole app look this way, now.
    @MainActor
    func apply() {
        #if canImport(UIKit)
        let style: UIUserInterfaceStyle = self == .dark ? .dark : self == .light ? .light : .unspecified
        for scene in UIApplication.shared.connectedScenes {
            for window in (scene as? UIWindowScene)?.windows ?? [] {
                window.overrideUserInterfaceStyle = style
            }
        }
        #elseif canImport(AppKit)
        NSApplication.shared.appearance = self == .dark ? NSAppearance(named: .darkAqua)
            : self == .light ? NSAppearance(named: .aqua) : nil
        #endif
    }
}

private struct AppliesAppearance: ViewModifier {
    @AppStorage(AppearanceChoice.storageKey) private var stored: AppearanceChoice = .system

    /// A debug launch's `-appearance=` pins the look for screenshots without touching the setting.
    private var choice: AppearanceChoice {
        Fixtures.appearance.flatMap(AppearanceChoice.init(rawValue:)) ?? stored
    }

    func body(content: Content) -> some View {
        content
            .onAppear { choice.apply() }
            .onChange(of: choice) { _, picked in picked.apply() }
    }
}

extension View {
    /// The person's appearance choice, on a window's root view. Every scene root carries it.
    func appliesAppearanceChoice() -> some View {
        modifier(AppliesAppearance())
    }
}

/// The choice itself, as Settings shows it.
struct AppearancePicker: View {
    @AppStorage(AppearanceChoice.storageKey) private var choice: AppearanceChoice = .system

    var body: some View {
        Picker("Appearance", selection: $choice) {
            ForEach(AppearanceChoice.allCases) { option in
                Text(option.label).tag(option)
            }
        }
        .pickerStyle(.segmented)
        .accessibilityIdentifier("appearance-picker")
    }
}
