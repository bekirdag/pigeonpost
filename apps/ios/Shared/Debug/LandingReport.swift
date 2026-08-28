//  Where a conversation came to rest, said out loud so a script can check it.
//
//  This exists because the obvious way to check it does not work. XCUITest reads the accessibility
//  hierarchy, and reading the accessibility hierarchy of a `LazyVStack` builds every row in it —
//  measured here, on the ninety-message fixture, the scroll view's content height went from 47,538
//  to 447,630 the moment a query ran, in the middle of the landing it was supposed to be observing.
//  A test that changes the layout by an order of magnitude while asking where the layout ended up
//  cannot answer the question, and it does not fail honestly either: the same build passed one run
//  and failed the next three.
//
//  So the app says where it is instead. Two numbers, from the two views whose relationship is the
//  whole question: the floor of the thread and the top of the composer. If the floor is at or above
//  the composer, the conversation is open at its end. If the floor is far below it — or never
//  reported at all, because a lazy stack does not build rows it is nowhere near — it is not.
//
//  `Tests/landing.sh` reads them. Debug only, and silent unless asked for by name.

import Foundation
import SwiftUI

enum LandingReport {
    static var enabled: Bool {
        #if DEBUG
        return Fixtures.enabled && CommandLine.arguments.contains("-report-landing")
        #else
        return false
        #endif
    }

    static func floor(_ y: CGFloat) {
        guard enabled else { return }
        NSLog("PIGEONPOST-LANDING floor=%.0f", y)
    }

    static func composer(_ y: CGFloat) {
        guard enabled else { return }
        NSLog("PIGEONPOST-LANDING composer=%.0f", y)
    }
}

/// Reports one edge of whatever it is attached to, in screen coordinates.
///
/// Nothing outside a debug build has this: the modifier is compiled away, so the shipped view tree
/// is the one it was before. iOS 18's `onGeometryChange` is what makes it cheap enough to leave on a
/// row — it reports when the value changes rather than on every layout pass.
struct ReportsItsPlace: ViewModifier {
    let measure: (CGRect) -> CGFloat
    let report: (CGFloat) -> Void

    @ViewBuilder
    func body(content: Content) -> some View {
        #if DEBUG
        if #available(iOS 18.0, macOS 15.0, *), LandingReport.enabled {
            content.onGeometryChange(for: CGFloat.self) { proxy in
                measure(proxy.frame(in: .global))
            } action: { now in
                report(now)
            }
        } else {
            content
        }
        #else
        content
        #endif
    }
}
