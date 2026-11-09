//! Untrusted file parsing for Nests (Phase B4, R-72).
//!
//! Every uploaded file is hostile until proven otherwise. This module turns one file into plain
//! text parts plus a manifest saying what was and was not read. It runs inside its own gVisor
//! sandbox (`pigeonpost-nest-parser`, no network, read-only input, a small output directory,
//! bounded memory and time), and it bounds itself as well, so a parser bug is contained twice.
//!
//! Rules that hold for every format:
//!
//! - The kind is decided from the bytes, never from the name or a client-supplied type.
//! - Output never uses a name from the input: parts are numbered (`part-0001.txt`), so an archive
//!   entry called `../../home/.ssh/authorized_keys` cannot place anything anywhere.
//! - Every expansion is bounded: total input, inflated bytes, archive entries, compression ratio,
//!   pages, output characters. Hitting a bound stops that part and is recorded, never ignored.
//! - Text is data. Nothing here interprets it; the runner later presents it to the model fenced and
//!   labelled as untrusted file content.

use serde::Serialize;
use std::io::Read;

/// Largest file accepted at all (the upload limit).
pub const MAX_INPUT_BYTES: usize = 25 * 1024 * 1024;
/// Most text produced from one file, all parts together, in characters.
pub const MAX_OUTPUT_CHARS: usize = 2_000_000;
/// Archives: most entries looked at, most bytes inflated in total, largest ratio for one entry.
pub const MAX_ZIP_ENTRIES: usize = 500;
pub const MAX_ZIP_TOTAL_BYTES: u64 = 100 * 1024 * 1024;
pub const MAX_ZIP_RATIO: u64 = 200;
/// PDF: most bytes inflated across all streams, most pages counted.
pub const MAX_PDF_INFLATE_BYTES: usize = 50 * 1024 * 1024;
pub const MAX_PDF_PAGES: usize = 2000;
/// CSV/XLSX: most rows and columns kept per sheet.
pub const MAX_ROWS: usize = 20_000;
pub const MAX_COLS: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Text,
    Csv,
    Pdf,
    Docx,
    Xlsx,
    Zip,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Everything was read.
    Parsed,
    /// Some of it was read; `warnings` and `coverage` say what was not.
    Partial,
    /// Nothing usable: unsupported, malformed or over a bound. `reason` says which.
    Rejected,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct Coverage {
    /// Pages, sheets or archive entries in the file.
    pub units_total: usize,
    /// How many of them yielded text.
    pub units_text: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Part {
    /// Numbered output name; never derived from the input.
    pub name: String,
    /// Where it came from, for the person (sanitized, display only).
    pub source: String,
    pub chars: usize,
    #[serde(skip)]
    pub text: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Manifest {
    pub kind: Kind,
    pub status: Status,
    pub reason: Option<String>,
    pub bytes: usize,
    pub coverage: Coverage,
    pub parts: Vec<Part>,
    pub warnings: Vec<String>,
}

impl Manifest {
    fn rejected(kind: Kind, bytes: usize, reason: &str) -> Manifest {
        Manifest {
            kind,
            status: Status::Rejected,
            reason: Some(reason.into()),
            bytes,
            coverage: Coverage::default(),
            parts: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

/// Collects parts under the output budget.
struct Out {
    parts: Vec<Part>,
    chars: usize,
    warnings: Vec<String>,
    full: bool,
}

impl Out {
    fn new() -> Out {
        Out {
            parts: Vec::new(),
            chars: 0,
            warnings: Vec::new(),
            full: false,
        }
    }

    fn push(&mut self, source: &str, text: String) {
        let text = clean(&text);
        if text.trim().is_empty() {
            return;
        }
        let room = MAX_OUTPUT_CHARS.saturating_sub(self.chars);
        if room == 0 {
            if !self.full {
                self.warnings
                    .push("output limit reached; later content was not read".into());
                self.full = true;
            }
            return;
        }
        let text: String = if text.chars().count() > room {
            self.warnings
                .push("output limit reached; text was cut".into());
            self.full = true;
            text.chars().take(room).collect()
        } else {
            text
        };
        let chars = text.chars().count();
        self.chars += chars;
        self.parts.push(Part {
            name: format!("part-{:04}.txt", self.parts.len() + 1),
            source: display_name(source),
            chars,
            text,
        });
    }
}

/// Control characters other than newline and tab removed; `\r\n` normalized.
fn clean(text: &str) -> String {
    text.replace("\r\n", "\n")
        .chars()
        .filter(|c| !c.is_control() || *c == '\n' || *c == '\t')
        .collect()
}

/// A name shown to the person, never used as a path: printable, at most 120 characters.
pub fn display_name(name: &str) -> String {
    let n: String = name.chars().filter(|c| !c.is_control()).take(120).collect();
    if n.is_empty() {
        "(unnamed)".into()
    } else {
        n
    }
}

/// Whether an archive entry name is one a careless extractor could be tricked by. Recorded as a
/// warning; nothing here ever writes an entry name to disk.
pub fn unsafe_entry_name(name: &str) -> bool {
    name.is_empty()
        || name.starts_with('/')
        || name.starts_with('\\')
        || name.contains('\0')
        || name.contains('\\')
        || name.split('/').any(|p| p == "..")
        || name.as_bytes().get(1) == Some(&b':')
}

/// Decide the kind from the bytes.
pub fn sniff(data: &[u8]) -> Kind {
    if data.starts_with(b"%PDF-") {
        return Kind::Pdf;
    }
    if data.starts_with(b"PK\x03\x04") || data.starts_with(b"PK\x05\x06") {
        if let Ok(mut z) = zip::ZipArchive::new(std::io::Cursor::new(data)) {
            let has =
                |z: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, n: &str| z.by_name(n).is_ok();
            if has(&mut z, "[Content_Types].xml") {
                if has(&mut z, "word/document.xml") {
                    return Kind::Docx;
                }
                if has(&mut z, "xl/workbook.xml") {
                    return Kind::Xlsx;
                }
            }
        }
        return Kind::Zip;
    }
    let sample = &data[..data.len().min(8192)];
    // A sample cut from a longer file may end inside a character; a whole file may not.
    let utf8 = match std::str::from_utf8(sample) {
        Ok(_) => true,
        Err(e) => data.len() > sample.len() && e.error_len().is_none(),
    };
    if utf8 && !sample.contains(&0) {
        if looks_like_csv(sample) {
            return Kind::Csv;
        }
        return Kind::Text;
    }
    Kind::Unknown
}

fn looks_like_csv(sample: &[u8]) -> bool {
    let text = String::from_utf8_lossy(sample);
    let lines: Vec<&str> = text.lines().take(5).collect();
    if lines.len() < 2 {
        return false;
    }
    let commas: Vec<usize> = lines.iter().map(|l| l.matches(',').count()).collect();
    commas[0] > 0 && commas.iter().all(|c| *c == commas[0])
}

/// Parse one file. Never panics on hostile input; every failure is a [`Status::Rejected`] manifest.
pub fn parse(data: &[u8]) -> Manifest {
    if data.len() > MAX_INPUT_BYTES {
        return Manifest::rejected(Kind::Unknown, data.len(), "file is larger than 25 MiB");
    }
    let kind = sniff(data);
    let result = std::panic::catch_unwind(|| match kind {
        Kind::Text => text_file(data),
        Kind::Csv => csv_file(data),
        Kind::Pdf => pdf_file(data),
        Kind::Docx => docx_file(data),
        Kind::Xlsx => xlsx_file(data),
        Kind::Zip => zip_file(data, 0),
        Kind::Unknown => Err("unsupported file type".to_string()),
    });
    match result {
        Ok(Ok((out, coverage))) => {
            let status = if out.parts.is_empty() {
                Status::Rejected
            } else if out.full
                || !out.warnings.is_empty()
                || coverage.units_text < coverage.units_total
            {
                Status::Partial
            } else {
                Status::Parsed
            };
            Manifest {
                kind,
                status,
                reason: out.parts.is_empty().then(|| "no readable text".into()),
                bytes: data.len(),
                coverage,
                parts: out.parts,
                warnings: out.warnings,
            }
        }
        Ok(Err(reason)) => Manifest::rejected(kind, data.len(), &reason),
        Err(_) => Manifest::rejected(kind, data.len(), "the file could not be parsed"),
    }
}

type Parsed = Result<(Out, Coverage), String>;

fn text_file(data: &[u8]) -> Parsed {
    let mut out = Out::new();
    out.push("text", String::from_utf8_lossy(data).into_owned());
    Ok((
        out,
        Coverage {
            units_total: 1,
            units_text: 1,
        },
    ))
}

fn csv_file(data: &[u8]) -> Parsed {
    let text = String::from_utf8_lossy(data);
    let mut out = Out::new();
    let mut rows = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if i >= MAX_ROWS {
            out.warnings
                .push(format!("only the first {MAX_ROWS} rows were read"));
            break;
        }
        let cols: Vec<&str> = line.split(',').take(MAX_COLS).collect();
        rows.push(cols.join("\t"));
    }
    out.push("table", rows.join("\n"));
    Ok((
        out,
        Coverage {
            units_total: 1,
            units_text: 1,
        },
    ))
}

/// Text inside `<tag …>…</tag>` elements, for the two Office XML parts we read. A small scanner
/// rather than an XML library: it only has to find text nodes, it allocates within the input's
/// size, and it cannot be talked into resolving entities or external references.
fn xml_text(xml: &str, tag: &str, paragraph: &str) -> String {
    let open = format!("<{tag}");
    let close = format!("</{tag}>");
    let para = format!("</{paragraph}>");
    let mut out = String::new();
    let mut rest = xml;
    loop {
        let next_text = rest.find(&open);
        let next_para = rest.find(&para);
        match (next_text, next_para) {
            (Some(t), p) if p.is_none_or(|p| t < p) => {
                let after = &rest[t + open.len()..];
                // `<w:t>` or `<w:t xml:space="preserve">`, not `<w:tab/>` or `<w:tbl>`.
                if !(after.starts_with('>') || after.starts_with(' ')) {
                    rest = after;
                    continue;
                }
                let Some(gt) = after.find('>') else { break };
                if after[..gt].ends_with('/') {
                    rest = &after[gt + 1..];
                    continue;
                }
                let body = &after[gt + 1..];
                let Some(end) = body.find(&close) else { break };
                out.push_str(&unescape(&body[..end]));
                rest = &body[end + close.len()..];
            }
            (_, Some(p)) => {
                out.push('\n');
                rest = &rest[p + para.len()..];
            }
            _ => break,
        }
    }
    out
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Read one archive member, bounded by `limit` inflated bytes.
fn read_member(
    z: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
    name: &str,
    limit: u64,
) -> Result<Option<String>, String> {
    let Ok(f) = z.by_name(name) else {
        return Ok(None);
    };
    let mut buf = Vec::new();
    f.take(limit + 1)
        .read_to_end(&mut buf)
        .map_err(|_| "a document part is damaged".to_string())?;
    if buf.len() as u64 > limit {
        return Err("a document part expands beyond the limit".into());
    }
    Ok(Some(String::from_utf8_lossy(&buf).into_owned()))
}

fn docx_file(data: &[u8]) -> Parsed {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|_| "damaged document")?;
    let xml =
        read_member(&mut z, "word/document.xml", 64 * 1024 * 1024)?.ok_or("not a Word document")?;
    let mut out = Out::new();
    out.push("document", xml_text(&xml, "w:t", "w:p"));
    let units = usize::from(!out.parts.is_empty());
    Ok((
        out,
        Coverage {
            units_total: 1,
            units_text: units,
        },
    ))
}

fn xlsx_file(data: &[u8]) -> Parsed {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|_| "damaged workbook")?;
    let shared: Vec<String> = match read_member(&mut z, "xl/sharedStrings.xml", 64 * 1024 * 1024)? {
        Some(xml) => xml
            .split("</si>")
            .map(|si| xml_text(si, "t", "zzz-none").trim().to_string())
            .collect(),
        None => Vec::new(),
    };
    let mut sheets: Vec<String> = (0..z.len())
        .filter_map(|i| {
            z.by_index(i)
                .ok()
                .and_then(|f| f.name().ok().map(|n| n.to_string()))
        })
        .filter(|n| n.starts_with("xl/worksheets/sheet") && n.ends_with(".xml"))
        .collect();
    sheets.sort();
    let mut out = Out::new();
    let mut cov = Coverage {
        units_total: sheets.len(),
        units_text: 0,
    };
    for (i, sheet) in sheets.iter().enumerate().take(64) {
        let Some(xml) = read_member(&mut z, sheet, 64 * 1024 * 1024)? else {
            continue;
        };
        let mut rows = Vec::new();
        for (r, row) in xml.split("</row>").enumerate() {
            if r >= MAX_ROWS {
                out.warnings.push(format!(
                    "sheet {}: only the first {MAX_ROWS} rows were read",
                    i + 1
                ));
                break;
            }
            let mut cells = Vec::new();
            for cell in row.split("<c ").skip(1).take(MAX_COLS) {
                let shared_ref = cell.split('>').next().unwrap_or("").contains("t=\"s\"");
                let inline = xml_text(cell, "t", "zzz-none");
                let value = xml_text(cell, "v", "zzz-none");
                let v = if shared_ref {
                    value
                        .trim()
                        .parse::<usize>()
                        .ok()
                        .and_then(|n| shared.get(n).cloned())
                        .unwrap_or_default()
                } else if !inline.is_empty() {
                    inline
                } else {
                    value
                };
                cells.push(v.trim().to_string());
            }
            if cells.iter().any(|c| !c.is_empty()) {
                rows.push(cells.join("\t"));
            }
        }
        if !rows.is_empty() {
            cov.units_text += 1;
            out.push(&format!("sheet {}", i + 1), rows.join("\n"));
        }
    }
    if sheets.len() > 64 {
        out.warnings
            .push("only the first 64 sheets were read".into());
    }
    Ok((out, cov))
}

fn zip_file(data: &[u8], depth: usize) -> Parsed {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(data)).map_err(|_| "damaged archive")?;
    let mut out = Out::new();
    let total = z.len();
    let mut cov = Coverage {
        units_total: total,
        units_text: 0,
    };
    if total > MAX_ZIP_ENTRIES {
        out.warnings.push(format!(
            "only the first {MAX_ZIP_ENTRIES} of {total} entries were looked at"
        ));
    }
    let mut inflated: u64 = 0;
    for i in 0..total.min(MAX_ZIP_ENTRIES) {
        let Ok(f) = z.by_index(i) else {
            out.warnings.push(format!("entry {} is damaged", i + 1));
            continue;
        };
        // A name that can't be decoded can't be checked for an unsafe path, so it is not read.
        let Ok(name) = f.name().map(|n| n.to_string()) else {
            out.warnings.push(format!(
                "entry {} has an unreadable name and was skipped",
                i + 1
            ));
            continue;
        };
        if f.is_dir() {
            cov.units_total -= 1;
            continue;
        }
        if unsafe_entry_name(&name) {
            out.warnings.push(format!(
                "entry {} has an unsafe path and was skipped",
                i + 1
            ));
            continue;
        }
        if f.is_symlink() {
            out.warnings
                .push(format!("entry {} is a link and was skipped", i + 1));
            continue;
        }
        let declared = f.size();
        let packed = f.compressed_size().max(1);
        if declared / packed > MAX_ZIP_RATIO {
            out.warnings.push(format!(
                "entry {} expands too much (possible archive bomb) and was skipped",
                i + 1
            ));
            continue;
        }
        if inflated + declared > MAX_ZIP_TOTAL_BYTES || declared > MAX_INPUT_BYTES as u64 {
            out.warnings
                .push("archive expands beyond the limit; the rest was not read".into());
            break;
        }
        // The declared size is the archive's claim, not a fact: read at most that much plus one
        // byte, so a lying header is caught instead of trusted.
        let mut buf = Vec::new();
        if f.take(declared + 1).read_to_end(&mut buf).is_err() || buf.len() as u64 > declared {
            out.warnings.push(format!(
                "entry {} is damaged or larger than it claims and was skipped",
                i + 1
            ));
            continue;
        }
        inflated += buf.len() as u64;
        let inner = sniff(&buf);
        let parsed = match inner {
            Kind::Zip | Kind::Docx | Kind::Xlsx if depth >= 1 => {
                out.warnings
                    .push(format!("entry {}: nested archives are not opened", i + 1));
                continue;
            }
            Kind::Zip => zip_file(&buf, depth + 1),
            Kind::Text => text_file(&buf),
            Kind::Csv => csv_file(&buf),
            Kind::Pdf => pdf_file(&buf),
            Kind::Docx => docx_file(&buf),
            Kind::Xlsx => xlsx_file(&buf),
            Kind::Unknown => {
                out.warnings
                    .push(format!("entry {} is not a supported type", i + 1));
                continue;
            }
        };
        match parsed {
            Ok((inner_out, _)) if !inner_out.parts.is_empty() => {
                cov.units_text += 1;
                for w in inner_out.warnings {
                    out.warnings.push(format!("entry {}: {w}", i + 1));
                }
                for p in inner_out.parts {
                    out.push(&format!("{name} / {}", p.source), p.text);
                }
            }
            Ok((inner_out, _)) => {
                for w in inner_out.warnings {
                    out.warnings.push(format!("entry {}: {w}", i + 1));
                }
                out.warnings
                    .push(format!("entry {} had no readable text", i + 1));
            }
            Err(e) => out.warnings.push(format!("entry {}: {e}", i + 1)),
        }
        if out.full {
            break;
        }
    }
    Ok((out, cov))
}

// ---- PDF -------------------------------------------------------------------------------------

/// A deliberately small PDF text extractor: inflate `FlateDecode` streams (bounded), read the
/// text-showing operators (`Tj`, `TJ`, `'`, `"`) from content streams, and count pages. It reads
/// ordinary text PDFs; scanned pages, custom font encodings and encrypted files yield no text and
/// show up as partial coverage rather than as garbage.
fn pdf_file(data: &[u8]) -> Parsed {
    if find(data, b"/Encrypt").is_some() {
        return Err("encrypted PDFs are not read".into());
    }
    let pages = count(data, b"/Type /Page") + count(data, b"/Type/Page")
        - count(data, b"/Type /Pages")
        - count(data, b"/Type/Pages");
    let pages = pages.min(MAX_PDF_PAGES);
    let mut out = Out::new();
    let mut inflated = 0usize;
    let mut texts = Vec::new();
    let mut pos = 0;
    while let Some(s) = find(&data[pos..], b"stream") {
        let start = pos + s + b"stream".len();
        // `endstream` itself contains `stream`; skip it.
        if s >= 3 && &data[pos + s - 3..pos + s] == b"end" {
            pos = start;
            continue;
        }
        let body_start = match data.get(start..start + 2) {
            Some(b"\r\n") => start + 2,
            Some([b'\n', _]) => start + 1,
            _ => start,
        };
        let Some(e) = find(&data[body_start..], b"endstream") else {
            break;
        };
        let raw = &data[body_start..body_start + e];
        let dict_start = data[..pos + s]
            .iter()
            .rposition(|b| *b == b'<')
            .unwrap_or(0);
        let dict = &data[dict_start..pos + s];
        let decoded: Option<Vec<u8>> = if find(dict, b"/FlateDecode").is_some() {
            let room = MAX_PDF_INFLATE_BYTES.saturating_sub(inflated);
            let mut buf = Vec::new();
            let ok = flate2::read::ZlibDecoder::new(raw)
                .take(room as u64 + 1)
                .read_to_end(&mut buf)
                .is_ok();
            if buf.len() > room {
                out.warnings
                    .push("PDF streams expand beyond the limit; the rest was not read".into());
                break;
            }
            inflated += buf.len();
            ok.then_some(buf)
        } else if find(dict, b"/Filter").is_none() {
            Some(raw.to_vec())
        } else {
            None
        };
        if let Some(content) = decoded {
            let t = pdf_text(&content);
            if !t.trim().is_empty() {
                texts.push(t);
            }
        }
        pos = body_start + e + b"endstream".len();
    }
    let joined = texts.join("\n");
    let printable = joined
        .chars()
        .filter(|c| c.is_alphanumeric() || c.is_whitespace() || c.is_ascii_punctuation())
        .count();
    if !joined.is_empty() && printable * 10 < joined.chars().count() * 7 {
        out.warnings
            .push("this PDF uses font encodings that could not be read as text".into());
        return Ok((
            out,
            Coverage {
                units_total: pages.max(1),
                units_text: 0,
            },
        ));
    }
    out.push("document", joined);
    let with_text = if out.parts.is_empty() {
        0
    } else {
        texts.len().min(pages.max(1))
    };
    if with_text < pages {
        out.warnings.push(format!(
            "text was found for about {with_text} of {pages} pages; the rest may be scanned images"
        ));
    }
    Ok((
        out,
        Coverage {
            units_total: pages.max(1),
            units_text: with_text,
        },
    ))
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn count(hay: &[u8], needle: &[u8]) -> usize {
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

/// Text-showing operators in one content stream.
fn pdf_text(content: &[u8]) -> String {
    let mut out = String::new();
    let mut i = 0;
    let mut pending = String::new();
    while i < content.len() {
        match content[i] {
            b'(' => {
                let (s, next) = pdf_literal(content, i);
                pending.push_str(&s);
                i = next;
            }
            b'<' if content.get(i + 1) != Some(&b'<') => {
                let end = content[i..]
                    .iter()
                    .position(|b| *b == b'>')
                    .map_or(content.len(), |e| i + e);
                let hex: Vec<u8> = content[i + 1..end]
                    .iter()
                    .copied()
                    .filter(u8::is_ascii_hexdigit)
                    .collect();
                let bytes: Vec<u8> = hex
                    .chunks(2)
                    .filter_map(|c| u8::from_str_radix(std::str::from_utf8(c).ok()?, 16).ok())
                    .collect();
                pending.push_str(&String::from_utf8_lossy(&bytes));
                i = end + 1;
            }
            b'T' if matches!(content.get(i + 1), Some(b'j' | b'J')) => {
                out.push_str(&pending);
                pending.clear();
                i += 2;
            }
            b'\'' | b'"' => {
                out.push('\n');
                out.push_str(&pending);
                pending.clear();
                i += 1;
            }
            b'T' if matches!(content.get(i + 1), Some(b'*' | b'd' | b'D')) => {
                out.push('\n');
                i += 2;
            }
            b'E' if content.get(i + 1) == Some(&b'T') => {
                pending.clear();
                out.push('\n');
                i += 2;
            }
            _ => i += 1,
        }
    }
    out
}

/// A PDF literal string starting at `start` (`(`): balanced parentheses and escapes.
fn pdf_literal(content: &[u8], start: usize) -> (String, usize) {
    let mut depth = 0usize;
    let mut i = start;
    let mut out = Vec::new();
    while i < content.len() {
        let b = content[i];
        match b {
            b'\\' => {
                i += 1;
                match content.get(i) {
                    Some(b'n') => out.push(b'\n'),
                    Some(b'r' | b't' | b'b' | b'f') => out.push(b' '),
                    Some(d @ b'0'..=b'7') => {
                        let mut v = u32::from(d - b'0');
                        for _ in 0..2 {
                            match content.get(i + 1) {
                                Some(d @ b'0'..=b'7') => {
                                    v = v * 8 + u32::from(d - b'0');
                                    i += 1;
                                }
                                _ => break,
                            }
                        }
                        out.push((v & 0xff) as u8);
                    }
                    Some(c) => out.push(*c),
                    None => break,
                }
            }
            b'(' => {
                if depth > 0 {
                    out.push(b);
                }
                depth += 1;
            }
            b')' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return (String::from_utf8_lossy(&out).into_owned(), i + 1);
                }
                out.push(b);
            }
            _ => out.push(b),
        }
        i += 1;
    }
    (String::from_utf8_lossy(&out).into_owned(), content.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn zip_of(entries: &[(&str, &[u8])], method: zip::CompressionMethod) -> Vec<u8> {
        let mut buf = std::io::Cursor::new(Vec::new());
        {
            let mut w = zip::ZipWriter::new(&mut buf);
            let opts = zip::write::SimpleFileOptions::default().compression_method(method);
            for (name, data) in entries {
                w.start_file(*name, opts).unwrap();
                w.write_all(data).unwrap();
            }
            w.finish().unwrap();
        }
        buf.into_inner()
    }

    fn all_text(m: &Manifest) -> String {
        m.parts
            .iter()
            .map(|p| p.text.as_str())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn text_csv_and_kind_from_bytes_not_names() {
        let m = parse(b"hello\r\nworld\x07");
        assert_eq!((m.kind, m.status), (Kind::Text, Status::Parsed));
        assert_eq!(all_text(&m), "hello\nworld");
        let m = parse(b"a,b,c\n1,2,3\n4,5,6\n");
        assert_eq!(m.kind, Kind::Csv);
        assert!(all_text(&m).contains("1\t2\t3"));
        assert_eq!(
            parse(&[0xff, 0xfe, 0x00, 0x01, 0x99]).status,
            Status::Rejected
        );
        assert_eq!(parse(&[0xff, 0xfe, 0x00, 0x9b]).status, Status::Rejected);
        assert_eq!(parse(b"text\0with nul").kind, Kind::Unknown);
        assert_eq!(parse(b"").status, Status::Rejected);
        let big = vec![b'a'; MAX_INPUT_BYTES + 1];
        assert_eq!(parse(&big).status, Status::Rejected);
    }

    #[test]
    fn office_documents_yield_their_text() {
        let ct: &[u8] = b"<Types/>";
        let doc = br#"<w:document><w:body><w:p><w:r><w:t>Quarterly</w:t></w:r><w:r><w:t xml:space="preserve"> plan &amp; budget</w:t></w:r><w:tab/></w:p><w:p><w:r><w:t>Line two</w:t></w:r></w:p></w:body></w:document>"#;
        let docx = zip_of(
            &[("[Content_Types].xml", ct), ("word/document.xml", doc)],
            zip::CompressionMethod::Deflated,
        );
        let m = parse(&docx);
        assert_eq!((m.kind, m.status), (Kind::Docx, Status::Parsed), "{m:?}");
        let t = all_text(&m);
        assert!(
            t.contains("Quarterly plan & budget") && t.contains("Line two"),
            "{t}"
        );

        let shared: &[u8] = br#"<sst><si><t>Name</t></si><si><t>Alice</t></si></sst>"#;
        let sheet: &[u8] = br#"<worksheet><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1"><v>42</v></c></row><row r="2"><c r="A2" t="s"><v>1</v></c><c r="B2" t="inlineStr"><is><t>x</t></is></c></row></sheetData></worksheet>"#;
        let xlsx = zip_of(
            &[
                ("[Content_Types].xml", ct),
                ("xl/workbook.xml", b"<workbook/>"),
                ("xl/sharedStrings.xml", shared),
                ("xl/worksheets/sheet1.xml", sheet),
            ],
            zip::CompressionMethod::Deflated,
        );
        let m = parse(&xlsx);
        assert_eq!((m.kind, m.status), (Kind::Xlsx, Status::Parsed), "{m:?}");
        let t = all_text(&m);
        assert!(t.contains("Name\t42") && t.contains("Alice\tx"), "{t}");
    }

    #[test]
    fn a_text_pdf_is_read_and_an_encrypted_one_refused() {
        let content =
            b"BT /F1 12 Tf 72 712 Td (Hello \\(PDF\\) world) Tj T* [(Sec) -20 (ond)] TJ ET";
        let mut z = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        z.write_all(content).unwrap();
        let stream = z.finish().unwrap();
        let mut pdf = b"%PDF-1.4\n1 0 obj << /Type /Pages /Count 1 >> endobj\n2 0 obj << /Type /Page >> endobj\n3 0 obj << /Length 99 /Filter /FlateDecode >>\nstream\n".to_vec();
        pdf.extend_from_slice(&stream);
        pdf.extend_from_slice(b"\nendstream\nendobj\n%%EOF");
        let m = parse(&pdf);
        assert_eq!(m.kind, Kind::Pdf);
        let t = all_text(&m);
        assert!(
            t.contains("Hello (PDF) world") && t.contains("Second"),
            "{t} {m:?}"
        );
        assert_eq!(
            m.coverage,
            Coverage {
                units_total: 1,
                units_text: 1
            }
        );
        let enc = b"%PDF-1.7\ntrailer << /Encrypt 5 0 R >>".to_vec();
        assert_eq!(
            parse(&enc).reason.as_deref(),
            Some("encrypted PDFs are not read")
        );
    }

    #[test]
    fn archive_traversal_names_never_become_paths_and_are_skipped() {
        let z = zip_of(
            &[
                (
                    "../../home/nest/.ssh/authorized_keys",
                    b"ssh-ed25519 AAAA attacker",
                ),
                ("/etc/passwd", b"root:x:0:0"),
                ("C:/Windows/evil.txt", b"evil"),
                ("dir\\..\\x.txt", b"evil"),
                ("notes/ok.txt", b"the real notes"),
            ],
            zip::CompressionMethod::Deflated,
        );
        let m = parse(&z);
        assert_eq!(m.kind, Kind::Zip);
        let t = all_text(&m);
        assert!(t.contains("the real notes"));
        assert!(
            !t.contains("attacker") && !t.contains("root:x") && !t.contains("evil"),
            "{t}"
        );
        assert_eq!(
            m.warnings
                .iter()
                .filter(|w| w.contains("unsafe path"))
                .count(),
            4
        );
        assert!(m
            .parts
            .iter()
            .all(|p| p.name.starts_with("part-") && !p.name.contains('/')));
        assert_eq!(m.status, Status::Partial);
    }

    #[test]
    fn archive_bombs_and_nesting_are_bounded() {
        // 50 MiB of zeros deflates to ~50 KiB: a ratio far over the limit.
        let zeros = vec![0u8; 50 * 1024 * 1024];
        let bomb = zip_of(&[("zeros.txt", &zeros)], zip::CompressionMethod::Deflated);
        let m = parse(&bomb);
        assert!(
            m.warnings.iter().any(|w| w.contains("archive bomb")),
            "{m:?}"
        );
        assert_eq!(m.status, Status::Rejected);
        // A zip inside a zip inside a zip: only one level is opened.
        let inner = zip_of(
            &[("deep.txt", b"deep text")],
            zip::CompressionMethod::Stored,
        );
        let middle = zip_of(&[("inner.zip", &inner)], zip::CompressionMethod::Stored);
        let outer = zip_of(
            &[("middle.zip", &middle), ("top.txt", b"top text")],
            zip::CompressionMethod::Stored,
        );
        let m = parse(&outer);
        let t = all_text(&m);
        assert!(t.contains("top text") && !t.contains("deep text"), "{t}");
        assert!(m.warnings.iter().any(|w| w.contains("nested archives")));
        // Too many entries: only the first MAX_ZIP_ENTRIES are looked at.
        let names: Vec<String> = (0..MAX_ZIP_ENTRIES + 5)
            .map(|i| format!("f{i}.txt"))
            .collect();
        let many: Vec<(&str, &[u8])> = names.iter().map(|n| (n.as_str(), &b"x"[..])).collect();
        let m = parse(&zip_of(&many, zip::CompressionMethod::Stored));
        assert!(m.warnings.iter().any(|w| w.contains("first 500")));
        assert_eq!(m.parts.len(), MAX_ZIP_ENTRIES);
    }

    #[test]
    fn hostile_bytes_never_panic() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        for len in [0usize, 1, 4, 30, 512, 4096] {
            for prefix in [&b"%PDF-1.4\nstream\n"[..], b"PK\x03\x04", b"", b"<w:t>"] {
                let mut data = prefix.to_vec();
                for _ in 0..len {
                    seed ^= seed << 13;
                    seed ^= seed >> 7;
                    seed ^= seed << 17;
                    data.push(seed as u8);
                }
                let _ = parse(&data);
            }
        }
    }
}
