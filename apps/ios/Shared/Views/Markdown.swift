//  Agent replies are markdown, so they are shown as markdown.
//
//  Not a full CommonMark implementation, and deliberately not: this renders what agents actually
//  send — headings, bullets, numbered lists, fenced code, block quotes, rules, tables, and inline
//  emphasis — and shows anything it does not understand as the literal text it was. Falling back to the raw
//  line is the important half: a renderer that silently drops what it cannot parse loses somebody's
//  message.
//
//  Nothing here acts on its own. No markup can cause the app to fetch, execute, or navigate
//  anywhere: a link is shown as a link and opens only when the person taps or clicks it, exactly as
//  `Text` treated markdown links before. A message body is another agent's text, and rendering it
//  must not turn it into instructions to this app — the same rule the plain-text path followed.
//
//  Prose — headings, paragraphs, lists, quotes — is drawn by `SelectableText`, the platform's own
//  read-only text view, so any part of a message can be selected and copied rather than all of it
//  or none. Consecutive prose blocks share one text view, so a selection can run across them.

import SwiftUI

struct MarkdownText: View {
    let raw: String
    /// Your own message's colours rather than a received one's.
    var onDark: Bool = false
    /// What the find bar is looking for. Every occurrence of it in this text is marked.
    var highlight: String = ""
    /// Whether this is the match the find bar is standing on. Every hit is marked; the one you
    /// stepped to is marked harder, the way a browser's find does it — otherwise "next" moves a
    /// scroll position and nothing else, and you have to work out which of six identical marks is
    /// the one it meant.
    var isCurrentMatch: Bool = false

    /// Headings and emphasis.
    private var strong: Color { onDark ? Theme.Mine.text : Theme.ink }
    /// The words themselves.
    private var primary: Color { onDark ? Theme.Mine.text : Theme.body }
    private var secondary: Color { onDark ? Theme.Mine.secondary : Theme.body }
    private var faint: Color { onDark ? Theme.Mine.meta : Theme.muted }
    private var panel: Color { onDark ? Theme.Mine.panel : Theme.Code.fill }
    private var panelStroke: Color { onDark ? Theme.Mine.stroke : Theme.Code.stroke }

    private var style: MarkdownStyle {
        MarkdownStyle(mine: onDark, highlight: highlight, isCurrentMatch: isCurrentMatch)
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            ForEach(Array(MarkdownSegment.segments(of: Markdown.blocks(of: raw)).enumerated()), id: \.offset) { _, segment in
                view(for: segment)
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .fixedSize(horizontal: false, vertical: true)
    }

    @ViewBuilder
    private func view(for segment: MarkdownSegment) -> some View {
        switch segment {
        case let .prose(blocks):
            SelectableText(text: style.prose(blocks))

        case let .code(text):
            // Horizontal scrolling rather than wrapping: wrapped code is a different program to
            // read, and agents send diffs and stack traces where the column matters.
            ScrollView(.horizontal, showsIndicators: false) {
                SelectableText(text: style.code(text), wraps: false)
            }
            .padding(.horizontal, 12)
            .padding(.vertical, 10)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(panel, in: RoundedRectangle(cornerRadius: 8))
            .overlay(RoundedRectangle(cornerRadius: 8).stroke(panelStroke, lineWidth: 1))

        case let .table(header, alignments, rows):
            // Scrolling sideways, for the reason the code block does: a table's width is its
            // columns', and a bubble is a fraction of a phone. Squeezing it to fit wraps every cell
            // to a word a line, and a table whose columns no longer line up is not a table.
            //
            // Indicators on, unlike the code block's: code that runs off the edge reads as code
            // that runs off the edge, but a truncated grid just looks like the wrong grid, so this
            // one has to say it can be moved.
            let width = max(header.count, rows.map(\.count).max() ?? 0)
            ScrollView(.horizontal, showsIndicators: true) {
                Grid(alignment: .leading, horizontalSpacing: 0, verticalSpacing: 0) {
                    GridRow {
                        ForEach(Array(0..<width), id: \.self) { column in
                            cell(at(header, column), align: alignment(alignments, column), isHeader: true)
                        }
                    }
                    ForEach(Array(rows.enumerated()), id: \.offset) { index, row in
                        GridRow {
                            ForEach(Array(0..<width), id: \.self) { column in
                                // Banded, so the eye keeps its place across a row it had to scroll
                                // to read.
                                cell(at(row, column), align: alignment(alignments, column),
                                     isHeader: false, shaded: !index.isMultiple(of: 2))
                            }
                        }
                    }
                }
                .clipShape(RoundedRectangle(cornerRadius: 8))
                .overlay(RoundedRectangle(cornerRadius: 8).stroke(panelStroke, lineWidth: 1))
                .padding(.bottom, 2)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
            // A cell is short; selecting one copies it.
            .textSelection(.enabled)

        case let .quote(text):
            HStack(alignment: .top, spacing: 10) {
                RoundedRectangle(cornerRadius: 1.5)
                    .fill(faint)
                    .frame(width: 3)
                SelectableText(text: style.quote(text))
            }

        case .rule:
            Rectangle()
                .fill(onDark ? Theme.Mine.rule : Theme.stroke)
                .frame(height: 1)
                .padding(.vertical, 1)
        }
    }

    /// One cell. A short row is padded out and a long one kept whole: the header says what shape
    /// the table is, but dropping the extra cell would drop somebody's data to make the shape true.
    private func at(_ row: [String], _ column: Int) -> String {
        row.indices.contains(column) ? row[column] : ""
    }

    /// Both `maxWidth` and `maxHeight` matter, and the width is the one that is easy to leave out.
    /// A `Grid` sizes each column to its widest cell and then places the narrower ones inside it at
    /// their own size — so a background applied without this fills the words rather than the cell,
    /// and the banding comes out as loose blocks with white gaps between the columns instead of
    /// stripes. Filling first, aligning inside the fill second, is what makes it read as a grid.
    private func cell(_ text: String, align: Alignment, isHeader: Bool, shaded: Bool = false) -> some View {
        inline(text)
            .font(.geist(13.5, isHeader ? .semibold : .regular))
            .foregroundStyle(isHeader ? strong : secondary)
            .padding(.horizontal, 9)
            .padding(.vertical, 5)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: align)
            .background(isHeader ? panel : (shaded ? panel.opacity(0.45) : Color.clear))
    }

    private func alignment(_ alignments: [Markdown.Column], _ column: Int) -> Alignment {
        switch alignments.indices.contains(column) ? alignments[column] : .leading {
        case .leading: return .leading
        case .center: return .center
        case .trailing: return .trailing
        }
    }

    /// Inline emphasis, via the system parser. On anything it refuses, the literal text is shown —
    /// an unparseable line is still somebody's sentence.
    private func inline(_ text: String) -> Text {
        if let attributed = try? AttributedString(
            markdown: text,
            options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        ) {
            return Text(marked(attributed))
        }
        return Text(marked(AttributedString(text)))
    }

    /// Mark every occurrence of the find term in a table cell. Prose is marked by `MarkdownStyle`.
    func marked(_ attributed: AttributedString) -> AttributedString {
        let needle = highlight.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return attributed }
        var out = attributed
        var searchFrom = out.startIndex
        while searchFrom < out.endIndex,
              let found = out[searchFrom...].range(of: needle, options: [.caseInsensitive]) {
            out[found].backgroundColor = isCurrentMatch
                ? Theme.found
                : Theme.found.opacity(0.28)
            if isCurrentMatch { out[found].foregroundColor = .white }
            searchFrom = found.upperBound
        }
        return out
    }
}

/// How the blocks are drawn: prose runs that share a text view, and the blocks that need a frame
/// of their own.
enum MarkdownSegment: Equatable {
    case prose([Markdown.Block])
    case code(String)
    case table(header: [String], alignments: [Markdown.Column], rows: [[String]])
    case quote(String)
    case rule

    static func segments(of blocks: [Markdown.Block]) -> [MarkdownSegment] {
        var out: [MarkdownSegment] = []
        var prose: [Markdown.Block] = []
        func flush() {
            if !prose.isEmpty { out.append(.prose(prose)); prose = [] }
        }
        for block in blocks {
            switch block {
            case .heading, .paragraph, .bullets, .numbered:
                prose.append(block)
            case let .code(text, _):
                flush(); out.append(.code(text))
            case let .table(header, alignments, rows):
                flush(); out.append(.table(header: header, alignments: alignments, rows: rows))
            case let .quote(text):
                flush(); out.append(.quote(text))
            case .rule:
                flush(); out.append(.rule)
            }
        }
        flush()
        return out
    }
}

#if canImport(UIKit)
private typealias PlatformColor = UIColor
#elseif canImport(AppKit)
private typealias PlatformColor = NSColor
#endif

/// The attributed text a message's prose is drawn with: the theme's faces and colours, inline
/// emphasis and code chips, links, and the find bar's marks.
struct MarkdownStyle {
    var mine = false
    var highlight = ""
    var isCurrentMatch = false

    static let bodySize: CGFloat = 14.5

    private var strong: PlatformColor { PlatformColor(mine ? Theme.Mine.text : Theme.ink) }
    private var primary: PlatformColor { PlatformColor(mine ? Theme.Mine.text : Theme.body) }
    private var secondary: PlatformColor { PlatformColor(mine ? Theme.Mine.secondary : Theme.body) }
    private var faint: PlatformColor { PlatformColor(mine ? Theme.Mine.meta : Theme.muted) }
    private var codeFill: PlatformColor { PlatformColor(mine ? Theme.Mine.panel : Theme.Code.fill) }
    private var codeText: PlatformColor { PlatformColor(mine ? Theme.Mine.panelText : Theme.Code.text) }

    /// Headings, paragraphs and lists, as one text.
    func prose(_ blocks: [Markdown.Block]) -> NSAttributedString {
        let out = NSMutableAttributedString()
        for (index, block) in blocks.enumerated() {
            let last = index == blocks.count - 1
            switch block {
            case let .heading(level, text):
                let size: CGFloat = level == 1 ? 19 : level == 2 ? 17 : 15.5
                out.append(inline(text, size: size, weight: .semibold, color: strong,
                                  paragraph: paragraph(after: last ? 0 : 8, before: index == 0 ? 0 : 4)))
            case let .paragraph(text):
                out.append(inline(text, size: Self.bodySize, color: primary, paragraph: paragraph(after: last ? 0 : 10)))
            case let .bullets(items):
                for (n, item) in items.enumerated() {
                    out.append(listItem(marker: "•", text: item, after: n == items.count - 1 ? (last ? 0 : 10) : 4))
                    if n < items.count - 1 { out.append(NSAttributedString(string: "\n")) }
                }
            case let .numbered(items):
                for (n, item) in items.enumerated() {
                    out.append(listItem(marker: "\(n + 1).", text: item, after: n == items.count - 1 ? (last ? 0 : 10) : 4))
                    if n < items.count - 1 { out.append(NSAttributedString(string: "\n")) }
                }
            default:
                continue
            }
            if !last { out.append(NSAttributedString(string: "\n")) }
        }
        return marked(out)
    }

    func quote(_ text: String) -> NSAttributedString {
        marked(inline(text, size: 14, color: secondary, paragraph: paragraph(after: 0)))
    }

    func code(_ text: String) -> NSAttributedString {
        let style = NSMutableParagraphStyle()
        style.lineSpacing = 3
        style.lineBreakMode = .byClipping
        return marked(NSAttributedString(string: text, attributes: [
            .font: Typeface.platform(12.5, mono: true),
            .foregroundColor: codeText,
            .paragraphStyle: style,
        ]))
    }

    private func paragraph(after: CGFloat, before: CGFloat = 0, indent: CGFloat = 0) -> NSParagraphStyle {
        let style = NSMutableParagraphStyle()
        // The design's line height is 1.45; Geist's own line box is about 1.26.
        style.lineSpacing = 3
        style.paragraphSpacing = after
        style.paragraphSpacingBefore = before
        if indent > 0 {
            style.headIndent = indent
            style.tabStops = [NSTextTab(textAlignment: .left, location: indent)]
            style.defaultTabInterval = indent
        }
        return style
    }

    private func listItem(marker: String, text: String, after: CGFloat) -> NSAttributedString {
        let indent: CGFloat = marker == "•" ? 16 : 22
        let style = paragraph(after: after, indent: indent)
        let line = NSMutableAttributedString(string: marker + "\t", attributes: [
            .font: Typeface.platform(marker == "•" ? Self.bodySize : 13.5, marker == "•" ? .regular : .medium),
            .foregroundColor: faint,
            .paragraphStyle: style,
        ])
        line.append(inline(text, size: Self.bodySize, color: primary, paragraph: style))
        return line
    }

    /// Inline emphasis, via the system parser. On anything it refuses, the literal text is shown —
    /// an unparseable line is still somebody's sentence.
    private func inline(_ text: String, size: CGFloat, weight: Font.Weight = .regular,
                        color: PlatformColor, paragraph: NSParagraphStyle) -> NSAttributedString {
        let base: [NSAttributedString.Key: Any] = [
            .font: Typeface.platform(size, weight), .foregroundColor: color, .paragraphStyle: paragraph,
        ]
        guard let parsed = try? AttributedString(
            markdown: text,
            options: .init(interpretedSyntax: .inlineOnlyPreservingWhitespace)
        ) else {
            return NSAttributedString(string: text, attributes: base)
        }
        let out = NSMutableAttributedString()
        for run in parsed.runs {
            let piece = String(parsed[run.range].characters)
            var attributes = base
            let intent = run.inlinePresentationIntent ?? []
            if intent.contains(.code) {
                attributes[.font] = Typeface.platform(size * 0.86, mono: true)
                attributes[.foregroundColor] = codeText
                attributes[.backgroundColor] = codeFill
            } else {
                let bold = intent.contains(.stronglyEmphasized)
                let italic = intent.contains(.emphasized)
                attributes[.font] = Typeface.platform(size, bold ? .semibold : weight, italic: italic && !bold)
                if bold { attributes[.foregroundColor] = strong }
            }
            if intent.contains(.strikethrough) {
                attributes[.strikethroughStyle] = NSUnderlineStyle.single.rawValue
            }
            if let link = run.link {
                attributes[.link] = link
                attributes[.foregroundColor] = PlatformColor(Theme.accent)
                attributes[.font] = Typeface.platform(size, .semibold)
            }
            out.append(NSAttributedString(string: piece, attributes: attributes))
        }
        return out
    }

    /// Mark every occurrence of the find term.
    ///
    /// On the text itself rather than on the message around it. A whole bubble tinted purple says
    /// "the word is somewhere in these forty lines", which on the messages this app carries is
    /// barely narrower than not having searched.
    ///
    /// Case-insensitive, and it walks the string rather than marking only the first: a paragraph
    /// that says the word four times has four hits in it, and marking one of them would be a
    /// different lie from marking none.
    private func marked(_ text: NSAttributedString) -> NSAttributedString {
        let needle = highlight.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !needle.isEmpty else { return text }
        let out = NSMutableAttributedString(attributedString: text)
        let string = out.string as NSString
        var from = 0
        while from < string.length {
            let found = string.range(of: needle, options: [.caseInsensitive],
                                     range: NSRange(location: from, length: string.length - from))
            guard found.location != NSNotFound, found.length > 0 else { break }
            let fill = PlatformColor(Theme.found)
            out.addAttribute(.backgroundColor, value: isCurrentMatch ? fill : fill.withAlphaComponent(0.28), range: found)
            // White on the strong fill, because the found colour is dark enough in the light
            // appearance to swallow ink-coloured text.
            if isCurrentMatch { out.addAttribute(.foregroundColor, value: PlatformColor.white, range: found) }
            from = NSMaxRange(found)
        }
        return out
    }
}

enum Markdown {
    /// Which way a column's cells sit, as the underline asked. Its own type rather than SwiftUI's
    /// `Alignment` so the block model stays a plain value the tests can compare.
    enum Column: Equatable { case leading, center, trailing }

    enum Block: Equatable {
        case heading(level: Int, text: String)
        case paragraph(String)
        case bullets([String])
        case numbered([String])
        case code(String, language: String?)
        case quote(String)
        case table(header: [String], alignments: [Column], rows: [[String]])
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

            // A table needs its underline. `| yes | no |` on its own is a line somebody typed, and
            // only the `|---|---|` beneath it says the pipes were a grid — so a sentence containing
            // a pipe stays the sentence it was. `lines.first` is already the line after this one.
            if trimmed.contains("|"), let next = lines.first, let alignments = tableAlignments(of: next) {
                flushAll()
                lines = lines.dropFirst() // the underline, now that it has been read
                let header = tableCells(of: trimmed)
                var rows: [[String]] = []
                // Rows run until a blank line or a line with no pipe in it.
                while let candidate = lines.first {
                    let row = candidate.trimmingCharacters(in: .whitespaces)
                    guard !row.isEmpty, row.contains("|") else { break }
                    lines = lines.dropFirst()
                    rows.append(tableCells(of: row))
                }
                blocks.append(.table(header: header, alignments: alignments, rows: rows))
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

    /// The cells of one row. A leading and a trailing pipe fence the row rather than being empty
    /// cells either side of it, so they come off before the split.
    static func tableCells(of line: String) -> [String] {
        var rest = Substring(line.trimmingCharacters(in: .whitespaces))
        if rest.hasPrefix("|") { rest = rest.dropFirst() }
        if rest.hasSuffix("|") { rest = rest.dropLast() }
        return rest.split(separator: "|", omittingEmptySubsequences: false)
            .map { $0.trimmingCharacters(in: .whitespaces) }
    }

    /// `|---|:--:|` and nothing else: every cell dashes, optionally anchored by a colon at one end
    /// or both. One alignment per column, or `nil` if this line is not a table underline — which is
    /// what keeps the row above it prose.
    static func tableAlignments(of line: String) -> [Column]? {
        let trimmed = line.trimmingCharacters(in: .whitespaces)
        guard trimmed.contains("|"), trimmed.contains("-") else { return nil }
        let cells = tableCells(of: trimmed)
        guard !cells.isEmpty else { return nil }
        var out: [Column] = []
        for cell in cells {
            var body = Substring(cell)
            let left = body.hasPrefix(":")
            if left { body = body.dropFirst() }
            let right = body.hasSuffix(":")
            if right { body = body.dropLast() }
            guard !body.isEmpty, body.allSatisfy({ $0 == "-" }) else { return nil }
            out.append(left && right ? .center : right ? .trailing : .leading)
        }
        return out
    }

    private static func numberedItem(of line: String) -> String? {
        let digits = line.prefix { $0.isNumber }
        guard !digits.isEmpty, digits.count <= 3 else { return nil }
        let rest = line.dropFirst(digits.count)
        guard rest.hasPrefix(". ") || rest.hasPrefix(") ") else { return nil }
        return String(rest.dropFirst(2))
    }
}
