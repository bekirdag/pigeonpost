//  Agent replies are markdown, so they are shown as markdown.
//
//  Not a full CommonMark implementation, and deliberately not: this renders what agents actually
//  send — headings, bullets, numbered lists, fenced code, block quotes, rules, tables, and inline
//  emphasis — and shows anything it does not understand as the literal text it was. Falling back to the raw
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
                            cell(at(header, column), isHeader: true)
                                .gridColumnAlignment(gridAlignment(alignments, column))
                        }
                    }
                    ForEach(Array(rows.enumerated()), id: \.offset) { index, row in
                        GridRow {
                            ForEach(Array(0..<width), id: \.self) { column in
                                // Banded, so the eye keeps its place across a row it had to scroll
                                // to read.
                                cell(at(row, column), isHeader: false, shaded: !index.isMultiple(of: 2))
                            }
                        }
                    }
                }
                .clipShape(RoundedRectangle(cornerRadius: 7))
                .overlay(RoundedRectangle(cornerRadius: 7).stroke(panel, lineWidth: 1))
                .padding(.bottom, 2)
            }
            .frame(maxWidth: .infinity, alignment: .leading)

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

    /// One cell. A short row is padded out and a long one kept whole: the header says what shape
    /// the table is, but dropping the extra cell would drop somebody's data to make the shape true.
    private func at(_ row: [String], _ column: Int) -> String {
        row.indices.contains(column) ? row[column] : ""
    }

    private func cell(_ text: String, isHeader: Bool, shaded: Bool = false) -> some View {
        inline(text)
            .font(.system(size: 13.5, weight: isHeader ? .semibold : .regular))
            .foregroundStyle(isHeader ? primary : secondary)
            .padding(.horizontal, 9)
            .padding(.vertical, 5)
            .frame(maxHeight: .infinity, alignment: .leading)
            .background(isHeader ? panel : (shaded ? panel.opacity(0.45) : Color.clear))
    }

    private func gridAlignment(_ alignments: [Markdown.Column], _ column: Int) -> HorizontalAlignment {
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
            return Text(attributed)
        }
        return Text(text)
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
