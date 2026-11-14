//  Lucide icons (ISC) drawn from the generated path data: no asset catalog, no SF Symbols, the same
//  strokes as the web kit. Stroke width scales with size (2 on the 24pt grid).

import SwiftUI

/// The outline of one icon on the 24 x 24 grid.
public struct PPIconShape: Shape {
    public let name: PPIconName

    public init(_ name: PPIconName) { self.name = name }

    public func path(in rect: CGRect) -> Path {
        let base = PPIconShape.path(for: name)
        let scale = min(rect.width, rect.height) / 24
        let dx = rect.minX + (rect.width - 24 * scale) / 2
        let dy = rect.minY + (rect.height - 24 * scale) / 2
        return base.applying(CGAffineTransform(a: scale, b: 0, c: 0, d: scale, tx: dx, ty: dy))
    }

    /// Parses absolute M/L/C/Z data (the only commands the generator emits).
    public static func path(for name: PPIconName) -> Path {
        parse(name.pathData)
    }

    static func parse(_ data: String) -> Path {
        var path = Path()
        let tokens = data.split(separator: " ")
        var i = 0
        func num() -> CGFloat {
            defer { i += 1 }
            return i < tokens.count ? CGFloat(Double(tokens[i]) ?? 0) : 0
        }
        while i < tokens.count {
            let cmd = tokens[i]
            i += 1
            switch cmd {
            case "M": path.move(to: CGPoint(x: num(), y: num()))
            case "L": path.addLine(to: CGPoint(x: num(), y: num()))
            case "C":
                let c1 = CGPoint(x: num(), y: num())
                let c2 = CGPoint(x: num(), y: num())
                path.addCurve(to: CGPoint(x: num(), y: num()), control1: c1, control2: c2)
            case "Z": path.closeSubpath()
            default: continue
            }
        }
        return path
    }
}

/// An icon, stroked in the current foreground style. Decorative by default; pass `label` when it
/// is the only content of a control.
public struct PPIcon: View {
    let name: PPIconName
    let size: CGFloat
    let weight: CGFloat
    let label: String?

    /// - Parameters:
    ///   - size: rendered size in points (16, 18, 20, 22 are the kit's common sizes).
    ///   - weight: stroke width on the 24pt grid (2 is standard; ticks use 2.2).
    public init(_ name: PPIconName, size: CGFloat = 18, weight: CGFloat = 2, label: String? = nil) {
        self.name = name
        self.size = size
        self.weight = weight
        self.label = label
    }

    public var body: some View {
        PPIconShape(name)
            .stroke(style: StrokeStyle(lineWidth: weight * size / 24, lineCap: .round, lineJoin: .round))
            .frame(width: size, height: size)
            .accessibilityHidden(label == nil)
            .accessibilityLabel(label.map { Text($0) } ?? Text(""))
    }
}

#Preview("Icons") {
    let names: [PPIconName] = [.inbox, .send, .reply, .paperclip, .phone, .video, .mic, .micOff, .bot, .sparkles,
                               .bird, .egg, .wallet, .coins, .creditCard, .badgeCheck, .settings, .checkCheck, .clock, .alertCircle]
    return LazyVGrid(columns: Array(repeating: GridItem(.fixed(40)), count: 5), spacing: 12) {
        ForEach(names, id: \.self) { PPIcon($0, size: 22).foregroundStyle(PP.Colors.text) }
    }
    .padding(24)
    .background(PP.Colors.surface)
}
