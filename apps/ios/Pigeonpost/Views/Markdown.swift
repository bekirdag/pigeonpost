//  Agent replies are markdown, so they are shown as markdown.
//
//  Not a full CommonMark implementation, and deliberately not: this renders what agents actually
//  send — headings, bullets, numbered lists, fenced code, block quotes, rules, and inline emphasis —
//  and shows anything it does not understand as the literal text it was. Falling back to the raw
//  line is the important half: a renderer that silently drops what it cannot parse loses somebody's
//  message.
//
//  Nothing here is interactive. Links are styled but not tappable, and no markup can cause the app
//  to fetch, execute, or navigate anywhere. A message body is another agent's text, and rendering
//  it must not turn it into instructions to this app — the same rule the plain-text path followed.

import SwiftUI

struct MarkdownText: View {
    let raw: String
    /// White-on-navy for your own bubble, ink on paper everywhere else.
    var onDark: Bool = false

    private var primary: Color { onDark ? .white : Theme.ink }
    private var secondary: Color { onDark ? .white.opacity(0.86) : Theme.body }
    private var faint: Color { onDark ? .white.opacity(0.6) : Theme.muted }
    private var panel: Color { onDark ? .white.opacity(0.14) : Theme.wash }

    var body: some View {
        VStack(alignment: .leading, spacing: 7) {
            ForEach(Array(Markdown.blocks(of: raw).enumerated()), id: \.offset) { _, block in
                view(for: block)
            }
        }
        .textSelection(.enabled)
        .fixedSize(horizontal: false, vertical: true)
    }

    @ViewBuilder
    private func view(for block: Markdown.Block) -> some View {
        switch block {
        case let .heading(level, text):
            inline(text)
                .font(.system(size: level == 1 ? 19 : level == 2 ? 17 : 15.5, weight: .bold))
                .foregroundStyle(primary)
                .padding(.top, 2)

        case let .paragraph(text):
            inline(text)
                .font(.system(size: 14.5))
                .foregroundStyle(primary)

        case let .bullets(items):
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(items.enumerated()), id: \.offset) { _, item in
                    HStack(alignment: .firstTextBaseline, spacing: 7) {
                        // A bullet that is not part of the text, so selecting the line copies the
                        // line rather than the decoration.
                        Text("•").foregroundStyle(faint)
                        inline(item)
                            .font(.system(size: 14.5))
                            .foregroundStyle(primary)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }

        case let .numbered(items):
            VStack(alignment: .leading, spacing: 4) {
                ForEach(Array(items.enumerated()), id: \.offset) { index, item in
                    HStack(alignment: .firstTextBaseline, spacing: 7) {
                        Text("\(index + 1).")
                            .font(.system(size: 13.5, weight: .medium))
                            .foregroundStyle(faint)
                        inline(item)
                            .font(.system(size: 14.5))
                            .foregroundStyle(primary)
                            .frame(maxWidth: .infinity, alignment: .leading)
                    }
                }
            }

        case let .code(text, _):
            // Horizontal scrolling rather than wrapping: wrapped code is a different program to
            // read, and agents send diffs and stack traces where the column matters.
            ScrollView(.horizontal, showsIndicators: false) {
                Text(text)
                    .font(.system(size: 12.5, design: .monospaced))
                    .foregroundStyle(secondary)
                    .textSelection(.enabled)
            }
            .padding(8)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(panel, in: RoundedRectangle(cornerRadius: 7))

        case let .quote(text):
            HStack(alignment: .top, spacing: 8) {
                RoundedRectangle(cornerRadius: 1.5)
                    .fill(faint)
                    .frame(width: 3)
                inline(text)
                    .font(.system(size: 14))
                    .foregroundStyle(secondary)
            }

        case .rule:
            Rectangle()
                .fill(onDark ? Color.white.opacity(0.25) : Theme.rule)
                .frame(height: 1)
                .padding(.vertical, 1)
        }
    }

    /// Inline emphasis, via the system parser. On anything it refuses, the literal text is shown —
    /// an unparseable line is still somebody's sentence.
    private func inline(_ text: String) -> Text {
        if let attributed = try? AttributedString(
            markdown: text,
            options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        ) {
            return Text(attributed)
        }
        return Text(text)
    }
}

enum Markdown {
    enum Block: Equatable {
        case heading(level: Int, text: String)
        case paragraph(String)
        case bullets([String])
        case numbered([String])
        case code(String, language: String?)
        case quote(String)
        case rule
    }

    /// Split a document into blocks. Line-based on purpose: it is predictable, it cannot loop, and
    /// the failure mode is "this became a paragraph" rather than "this vanished".
    static func blocks(of raw: String) -> [Block] {
        var blocks: [Block] = []
        var paragraph: [String] = []
        var bullets: [String] = []
        var numbered: [String] = []

        func flushParagraph() {
            if !paragraph.isEmpty {
                blocks.append(.paragraph(paragraph.joined(separator: "\n")))
                paragraph = []
            }
        }
        func flushLists() {
            if !bullets.isEmpty {
                blocks.append(.bullets(bullets))
                bullets = []
            }
            if !numbered.isEmpty {
                blocks.append(.numbered(numbered))
                numbered = []
            }
        }
        func flushAll() {
            flushParagraph()
            flushLists()
        }

        var lines = raw.components(separatedBy: .newlines)[...]
        while let line = lines.first {
            lines = lines.dropFirst()
            let trimmed = line.trimmingCharacters(in: .whitespaces)

            // Fenced code first: everything inside it is literal, including things that would
            // otherwise look like headings or bullets.
            if trimmed.hasPrefix("```") {
                flushAll()
                let language = String(trimmed.dropFirst(3)).trimmingCharacters(in: .whitespaces)
                var body: [String] = []
                while let next = lines.first {
                    lines = lines.dropFirst()
                    if next.trimmingCharacters(in: .whitespaces).hasPrefix("```") { break }
                    body.append(next)
                }
                // An unterminated fence is not an error: the rest of the message is the block.
                blocks.append(.code(body.joined(separator: "\n"), language: language.isEmpty ? nil : language))
                continue
            }

            if trimmed.isEmpty {
                flushAll()
                continue
            }

            // `---` is a rule only on its own line; a line of dashes under text is a setext
            // heading in CommonMark, which agents do not write and which would eat the paragraph.
            if (trimmed == "---" || trimmed == "***" || trimmed == "___") && paragraph.isEmpty {
                flushAll()
                blocks.append(.rule)
                continue
            }

            if let heading = heading(of: trimmed) {
                flushAll()
                blocks.append(heading)
                continue
            }

            if trimmed.hasPrefix("> ") || trimmed == ">" {
                flushAll()
                blocks.append(.quote(String(trimmed.dropFirst(1)).trimmingCharacters(in: .whitespaces)))
                continue
            }

            if let item = bulletItem(of: trimmed) {
                flushParagraph()
                if !numbered.isEmpty { flushLists() }
                bullets.append(item)
                continue
            }

            if let item = numberedItem(of: trimmed) {
                flushParagraph()
                if !bullets.isEmpty { flushLists() }
                numbered.append(item)
                continue
            }

            flushLists()
            paragraph.append(line)
        }
        flushAll()
        return blocks
    }

    private static func heading(of line: String) -> Block? {
        var level = 0
        var rest = Substring(line)
        while rest.first == "#" && level < 6 {
            level += 1
            rest = rest.dropFirst()
        }
        // `#text` is not a heading — it is a hashtag, or a colour, or an issue number.
        guard level > 0, rest.first == " " else { return nil }
        return .heading(level: level, text: rest.trimmingCharacters(in: .whitespaces))
    }

    private static func bulletItem(of line: String) -> String? {
        for marker in ["- ", "* ", "+ "] where line.hasPrefix(marker) {
            return String(line.dropFirst(marker.count))
        }
        return nil
    }

    private static func numberedItem(of line: String) -> String? {
        let digits = line.prefix { $0.isNumber }
        guard !digits.isEmpty, digits.count <= 3 else { return nil }
        let rest = line.dropFirst(digits.count)
        guard rest.hasPrefix(". ") || rest.hasPrefix(") ") else { return nil }
        return String(rest.dropFirst(2))
    }
}
