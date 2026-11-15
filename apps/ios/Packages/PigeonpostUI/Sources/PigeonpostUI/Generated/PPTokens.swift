// Pigeonpost tokens 1.0.0 — generated from css/tokens.css. Do not edit by hand.
import SwiftUI

private extension Color {
    init(hex: UInt32) { self.init(.sRGB, red: Double((hex >> 16) & 0xFF) / 255, green: Double((hex >> 8) & 0xFF) / 255, blue: Double(hex & 0xFF) / 255, opacity: 1) }
    static func dynamic(light: UInt32, dark: UInt32) -> Color {
        #if canImport(UIKit)
        return Color(UIColor { $0.userInterfaceStyle == .dark ? UIColor(Color(hex: dark)) : UIColor(Color(hex: light)) })
        #else
        return Color(NSColor(name: nil) { $0.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? NSColor(Color(hex: dark)) : NSColor(Color(hex: light)) })
        #endif
    }
}

public enum PP {
    public enum Colors {
        public static let bg = Color.dynamic(light: 0xF4F6F8, dark: 0x0B0E12)
        public static let bgSubtle = Color.dynamic(light: 0xFAFBFC, dark: 0x0F1217)
        public static let surface = Color.dynamic(light: 0xFFFFFF, dark: 0x141820)
        public static let surfaceRaised = Color.dynamic(light: 0xFFFFFF, dark: 0x1A1F28)
        public static let surfaceSunken = Color.dynamic(light: 0xF4F6F8, dark: 0x10141A)
        public static let surfaceHover = Color.dynamic(light: 0xEEF1F5, dark: 0x1D232D)
        public static let surfaceSelected = Color.dynamic(light: 0xE4EBF7, dark: 0x1C2840)
        public static let border = Color.dynamic(light: 0xE1E5EA, dark: 0x232A35)
        public static let borderSubtle = Color.dynamic(light: 0xECEEF2, dark: 0x1C222B)
        public static let borderStrong = Color.dynamic(light: 0xC9CFD8, dark: 0x343D4A)
        public static let text = Color.dynamic(light: 0x12161C, dark: 0xF2F4F7)
        public static let textBody = Color.dynamic(light: 0x343C47, dark: 0xD3D8E0)
        public static let textMuted = Color.dynamic(light: 0x59626E, dark: 0xA3ACB9)
        public static let textFaint = Color.dynamic(light: 0x636C78, dark: 0x8A93A0)
        public static let textInverse = Color.dynamic(light: 0xFFFFFF, dark: 0x0B0E12)
        public static let textLink = Color.dynamic(light: 0x0A5FE0, dark: 0x6EA8FF)
        public static let accent = Color.dynamic(light: 0x086CFF, dark: 0x086CFF)
        public static let accentHover = Color.dynamic(light: 0x0A5FE0, dark: 0x0A5FE0)
        public static let accentPressed = Color.dynamic(light: 0x0B4FB8, dark: 0x0B4FB8)
        public static let accentSubtle = Color.dynamic(light: 0xEDF4FF, dark: 0x13233F)
        public static let accentSubtleBorder = Color.dynamic(light: 0xB3D0FF, dark: 0x23406F)
        public static let onAccent = Color.dynamic(light: 0xFFFFFF, dark: 0xFFFFFF)
        public static let onSolid = Color.dynamic(light: 0xFFFFFF, dark: 0xFFFFFF)
        public static let surfaceInverse = Color.dynamic(light: 0x1A1F27, dark: 0xF2F4F7)
        public static let onInverse = Color.dynamic(light: 0xFFFFFF, dark: 0x0B0E12)
        public static let controlThumb = Color.dynamic(light: 0xFFFFFF, dark: 0xF2F4F7)
        public static let brand = Color.dynamic(light: 0x05368B, dark: 0x8FB4FF)
        public static let success = Color.dynamic(light: 0x15801D, dark: 0x4FDB55)
        public static let successFill = Color.dynamic(light: 0xECFBEE, dark: 0x10261A)
        public static let successBorder = Color.dynamic(light: 0xA6EBAB, dark: 0x1E4A2B)
        public static let successSolid = Color.dynamic(light: 0x15801D, dark: 0x26CE2D)
        public static let warning = Color.dynamic(light: 0x8A5208, dark: 0xF5B547)
        public static let warningFill = Color.dynamic(light: 0xFDF3E1, dark: 0x2A2010)
        public static let warningBorder = Color.dynamic(light: 0xF0D6A4, dark: 0x4D3A16)
        public static let warningSolid = Color.dynamic(light: 0xB45309, dark: 0xF5B547)
        public static let danger = Color.dynamic(light: 0xC42B2B, dark: 0xFF6B6B)
        public static let dangerFill = Color.dynamic(light: 0xFDF0EF, dark: 0x2B1416)
        public static let dangerBorder = Color.dynamic(light: 0xF5C2CC, dark: 0x5A2328)
        public static let dangerSolid = Color.dynamic(light: 0xC42B2B, dark: 0xC42B2B)
        public static let info = Color.dynamic(light: 0x0B4FB8, dark: 0x6EA8FF)
        public static let infoFill = Color.dynamic(light: 0xEDF4FF, dark: 0x13233F)
        public static let infoBorder = Color.dynamic(light: 0xB3D0FF, dark: 0x23406F)
        public static let infoSolid = Color.dynamic(light: 0x086CFF, dark: 0x086CFF)
        public static let bubbleMine = Color.dynamic(light: 0xEAF1FE, dark: 0x17325E)
        public static let bubbleMineBorder = Color.dynamic(light: 0xC8D9FA, dark: 0x23477F)
        public static let bubbleMineText = Color.dynamic(light: 0x13203B, dark: 0xEAF1FE)
        public static let bubbleTheirs = Color.dynamic(light: 0xFFFFFF, dark: 0x1A1F28)
        public static let bubbleTheirsBorder = Color.dynamic(light: 0xE1E5EA, dark: 0x262D38)
        public static let bubbleTheirsText = Color.dynamic(light: 0x12161C, dark: 0xF2F4F7)
        public static let bubbleAgentRail = Color.dynamic(light: 0x26CE2D, dark: 0x26CE2D)
        public static let tickSent = Color.dynamic(light: 0x7C8594, dark: 0x8A93A0)
        public static let tickSeen = Color.dynamic(light: 0x1E8A50, dark: 0x4FDB55)
        public static let tickWorking = Color.dynamic(light: 0x1DA824, dark: 0x1DA824)
    }
    public enum Palette {
        public static let navy50 = Color(hex: 0xEEF2FA)
        public static let navy100 = Color(hex: 0xD6E0F2)
        public static let navy200 = Color(hex: 0xAEC1E6)
        public static let navy300 = Color(hex: 0x7F9BD3)
        public static let navy400 = Color(hex: 0x4F72BD)
        public static let navy500 = Color(hex: 0x2550A6)
        public static let navy600 = Color(hex: 0x0F4199)
        public static let navy700 = Color(hex: 0x05368B)
        public static let navy800 = Color(hex: 0x052B6E)
        public static let navy900 = Color(hex: 0x04204F)
        public static let navy950 = Color(hex: 0x021533)
        public static let blue50 = Color(hex: 0xEDF4FF)
        public static let blue100 = Color(hex: 0xD8E7FF)
        public static let blue200 = Color(hex: 0xB3D0FF)
        public static let blue300 = Color(hex: 0x80B1FF)
        public static let blue400 = Color(hex: 0x4A90FF)
        public static let blue500 = Color(hex: 0x086CFF)
        public static let blue600 = Color(hex: 0x0A5FE0)
        public static let blue700 = Color(hex: 0x0B4FB8)
        public static let blue800 = Color(hex: 0x0D418F)
        public static let blue900 = Color(hex: 0x0E3570)
        public static let blue950 = Color(hex: 0x0A2147)
        public static let green50 = Color(hex: 0xECFBEE)
        public static let green100 = Color(hex: 0xD1F5D4)
        public static let green200 = Color(hex: 0xA6EBAB)
        public static let green300 = Color(hex: 0x6FDD77)
        public static let green400 = Color(hex: 0x4FDB55)
        public static let green500 = Color(hex: 0x26CE2D)
        public static let green600 = Color(hex: 0x1DA824)
        public static let green700 = Color(hex: 0x15801D)
        public static let green800 = Color(hex: 0x136419)
        public static let green900 = Color(hex: 0x114F16)
        public static let green950 = Color(hex: 0x072B0A)
        public static let gray0 = Color(hex: 0xFFFFFF)
        public static let gray25 = Color(hex: 0xFAFBFC)
        public static let gray50 = Color(hex: 0xF4F6F8)
        public static let gray100 = Color(hex: 0xECEFF3)
        public static let gray200 = Color(hex: 0xDFE3E9)
        public static let gray300 = Color(hex: 0xC9CFD8)
        public static let gray400 = Color(hex: 0x9AA3B0)
        public static let gray500 = Color(hex: 0x6B7482)
        public static let gray600 = Color(hex: 0x4E5663)
        public static let gray700 = Color(hex: 0x343C47)
        public static let gray800 = Color(hex: 0x1F252E)
        public static let gray850 = Color(hex: 0x181C23)
        public static let gray900 = Color(hex: 0x12161C)
        public static let gray950 = Color(hex: 0x0B0E12)
        public static let red50 = Color(hex: 0xFDF0EF)
        public static let red200 = Color(hex: 0xF5C2CC)
        public static let red400 = Color(hex: 0xFF6B6B)
        public static let red600 = Color(hex: 0xC42B2B)
        public static let red700 = Color(hex: 0xA3243F)
        public static let amber50 = Color(hex: 0xFDF3E1)
        public static let amber200 = Color(hex: 0xF0D6A4)
        public static let amber400 = Color(hex: 0xF5B547)
        public static let amber600 = Color(hex: 0xB45309)
        public static let amber700 = Color(hex: 0x8A5208)
        public static let violet50 = Color(hex: 0xF1EEFD)
        public static let violet400 = Color(hex: 0x9A85F2)
        public static let violet600 = Color(hex: 0x5735B0)
        public static let teal50 = Color(hex: 0xE6F7F4)
        public static let teal400 = Color(hex: 0x3FC7B4)
        public static let teal600 = Color(hex: 0x0D6458)
        public static let chart1 = Color(hex: 0x086CFF)
        public static let chart2 = Color(hex: 0x26CE2D)
        public static let chart3 = Color(hex: 0x05368B)
        public static let chart4 = Color(hex: 0xF5B547)
        public static let chart5 = Color(hex: 0x9A85F2)
        public static let chart6 = Color(hex: 0x3FC7B4)
        public static let chart7 = Color(hex: 0xFF6B6B)
        public static let chart8 = Color(hex: 0x6B7482)
    }
    public enum Space {
        public static let s0: CGFloat = 0
        public static let s1: CGFloat = 4
        public static let s2: CGFloat = 8
        public static let s3: CGFloat = 12
        public static let s4: CGFloat = 16
        public static let s5: CGFloat = 20
        public static let s6: CGFloat = 24
        public static let s8: CGFloat = 32
        public static let s10: CGFloat = 40
        public static let s12: CGFloat = 48
        public static let s16: CGFloat = 64
        public static let s20: CGFloat = 80
        public static let s24: CGFloat = 96
    }
    public enum Radius {
        public static let xs: CGFloat = 4
        public static let s: CGFloat = 8
        public static let m: CGFloat = 10
        public static let l: CGFloat = 14
        public static let xl: CGFloat = 18
        public static let x2xl: CGFloat = 24
        public static let full: CGFloat = 999
        public static let bubble: CGFloat = 18
        public static let bubbletail: CGFloat = 6
    }
    public enum Font {
        public static let displayXl = SwiftUI.Font.custom("Geist", size: 56).weight(.semibold)
        public static let displayL = SwiftUI.Font.custom("Geist", size: 44).weight(.semibold)
        public static let displayM = SwiftUI.Font.custom("Geist", size: 36).weight(.semibold)
        public static let h1 = SwiftUI.Font.custom("Geist", size: 28).weight(.semibold)
        public static let h2 = SwiftUI.Font.custom("Geist", size: 22).weight(.semibold)
        public static let h3 = SwiftUI.Font.custom("Geist", size: 18).weight(.semibold)
        public static let h4 = SwiftUI.Font.custom("Geist", size: 16).weight(.semibold)
        public static let bodyL = SwiftUI.Font.custom("Geist", size: 17).weight(.regular)
        public static let body = SwiftUI.Font.custom("Geist", size: 15).weight(.regular)
        public static let bodyS = SwiftUI.Font.custom("Geist", size: 13.5).weight(.regular)
        public static let caption = SwiftUI.Font.custom("Geist", size: 12).weight(.regular)
        public static let overline = SwiftUI.Font.custom("Geist", size: 11.5).weight(.semibold)
        public static let mono = SwiftUI.Font.custom("Geist Mono", size: 14)
    }
}
