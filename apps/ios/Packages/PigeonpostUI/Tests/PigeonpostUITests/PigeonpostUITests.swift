import XCTest
import SwiftUI
@testable import PigeonpostUI

final class TokenTests: XCTestCase {
    func testSpacingAndRadii() {
        XCTAssertEqual(PP.Space.s4, 16)
        XCTAssertEqual(PP.Space.s6, 24)
        XCTAssertEqual(PP.Radius.m, 10)
        XCTAssertEqual(PP.Radius.l, 14)
        XCTAssertEqual(PP.Radius.bubble, 18)
        XCTAssertEqual(PP.Radius.bubbletail, 6)
        XCTAssertEqual(PP.Control.s, 30)
        XCTAssertEqual(PP.Control.m, 38)
        XCTAssertEqual(PP.Control.l, 46)
        XCTAssertEqual(PP.Control.touchMin, 44)
    }

    func testTypeRoles() {
        XCTAssertEqual(PPTextRole.body.size, 15)
        XCTAssertEqual(PPTextRole.body.lineHeight, 22)
        XCTAssertEqual(PPTextRole.h1.size, 28)
        XCTAssertEqual(PPTextRole.h1.lineHeight, 34)
        XCTAssertEqual(PPTextRole.h1.weight, .semibold)
        XCTAssertEqual(PPTextRole.caption.lineHeight, 16)
    }

    func testAccentResolvesInBothSchemes() {
        var light = EnvironmentValues(); light.colorScheme = .light
        var dark = EnvironmentValues(); dark.colorScheme = .dark
        let l = PP.Colors.accent.resolve(in: light)
        XCTAssertEqual(Int((l.red * 255).rounded()), 0x08)
        XCTAssertEqual(Int((l.green * 255).rounded()), 0x6C)
        XCTAssertEqual(Int((l.blue * 255).rounded()), 0xFF)
        let bg = PP.Colors.bg.resolve(in: dark)
        XCTAssertEqual(Int((bg.red * 255).rounded()), 0x0B)
    }

    func testBubbleCorners() {
        let mine = PPMessageBubble.radii(role: .mine, position: .single)
        XCTAssertEqual(mine.bottomTrailing, 6)
        XCTAssertEqual(mine.topTrailing, 18)
        XCTAssertEqual(mine.bottomLeading, 18)
        let theirsMiddle = PPMessageBubble.radii(role: .theirs, position: .middle)
        XCTAssertEqual(theirsMiddle.topLeading, 6)
        XCTAssertEqual(theirsMiddle.bottomLeading, 6)
        XCTAssertEqual(theirsMiddle.topTrailing, 18)
        let agentFirst = PPMessageBubble.radii(role: .agent, position: .first)
        XCTAssertEqual(agentFirst.topLeading, 18)
        XCTAssertEqual(agentFirst.bottomLeading, 6)
    }
}

final class PeerTests: XCTestCase {
    func testDisplayNames() {
        XCTAssertEqual(PPPeer.displayName("/bekir/main"), "Bekir")
        XCTAssertEqual(PPPeer.displayName("/wodo/hr"), "Wodo HR")
        XCTAssertEqual(PPPeer.displayName("/acme/john_doe"), "Acme John Doe")
        XCTAssertEqual(PPPeer.displayName("/github/alex"), "/github/alex")
        XCTAssertEqual(PPPeer.displayName("/main"), "Main")
        XCTAssertEqual(PPPeer.displayName("/k/aaaa1111bbbb2222cccc3333dd"), "/k/aaaa1111b…")
        XCTAssertEqual(PPPeer.displayName("Team inbox"), "Team inbox")
    }

    func testInitials() {
        XCTAssertEqual(PPPeer.initials("/bekir/main"), "BE")
        XCTAssertEqual(PPPeer.initials("/wodo/hr"), "WH")
        XCTAssertEqual(PPPeer.initials("/acme/john-doe"), "AD")
        XCTAssertEqual(PPPeer.initials("/bekir/main", name: "Mum"), "MU")
        XCTAssertEqual(PPPeer.initials("/bekir/agent1"), "BA")
        XCTAssertEqual(PPPeer.initials(nil, name: "Özge Arıç"), "ÖA")
        XCTAssertEqual(PPPeer.initials(""), "UN")
    }

    func testToneHashMatchesWebApp() {
        XCTAssertEqual(PPPeer.toneIndex("/bekir/agent1"), 6)
        XCTAssertEqual(PPPeer.toneIndex("/bekir/docdex"), 1)
        XCTAssertEqual(PPPeer.toneIndex("/k/eeee5555ffff6666gggg7777hh"), 6)
        XCTAssertEqual(PPPeer.toneIndex(nil), 1)
        XCTAssertEqual(PPTone.forPeer("/bekir/docdex"), .navy)
        XCTAssertEqual(PPTone.forPeer("/bekir/agent1"), .red)
    }
}

final class IconTests: XCTestCase {
    func testEveryIconParses() {
        XCTAssertEqual(PPIconName.allCases.count, 181)
        for name in PPIconName.allCases {
            let rect = PPIconShape.path(for: name).boundingRect
            XCTAssertFalse(rect.isEmpty && rect.width == 0 && rect.height == 0, "\(name) is empty")
            XCTAssertTrue(CGRect(x: -1, y: -1, width: 26, height: 26).contains(rect), "\(name) leaves the 24pt grid: \(rect)")
        }
    }

    func testCheckIsThreeSegments() {
        XCTAssertEqual(PPIconName.check.pathData, "M 20 6 L 9 17 L 4 12")
    }

    func testFontsRegister() {
        XCTAssertTrue(PPFonts.register())
        XCTAssertTrue(PPFonts.isAvailable)
    }
}
