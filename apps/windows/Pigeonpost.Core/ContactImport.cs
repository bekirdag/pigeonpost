using System.Security.Cryptography;
using System.Text;
using System.Text.RegularExpressions;

namespace Pigeonpost.Core;

// A person read from an exported address book. Names stay on this device (docs/contacts-and-phone.md §1).
public sealed record ImportedContact(string Name, IReadOnlyList<string> Phones);

// One normalised number from the import: the name it was filed under, the E.164 form and its discovery hash.
public sealed record ImportedNumber(string Name, string Phone, string Hash);

public sealed record ContactImportResult(IReadOnlyList<ImportedNumber> Numbers, int Unreadable, int People);

// A port of site-inbox/contacts.js (normalizeE164, parseVcard, parseCsv, inviteName) so every client reads the same
// exports the same way. Deliberately simpler than libphonenumber: a number it gets wrong simply fails to match.
public static class ContactImport
{
    public const int MaxFileBytes = 5 * 1024 * 1024;
    private const string HashPrefix = "pigeonpost:phone:v1:";

    public static string? NormalizeE164(string? raw, string? defaultCallingCode)
    {
        if (raw is null) return null;
        var s = raw.Trim();
        s = Regex.Replace(s, "^tel:", "", RegexOptions.IgnoreCase);
        s = s.Split(';')[0];
        s = Regex.Replace(s, @"^(\+\d{1,3})[\s.-]*\(0\)", "$1");
        s = Regex.Replace(s, @"\s*(?:ext(?:ension)?\.?|x|#)\s*\d*\s*$", "", RegexOptions.IgnoreCase);
        s = Regex.Replace(s, @"[\s.\-() ]", "");
        if (!Regex.IsMatch(s, @"\A\+?[0-9]+\z")) return null;
        var cc = Regex.Replace(defaultCallingCode ?? "", "[^0-9]", "");
        string digits;
        if (s.StartsWith('+')) digits = s[1..];
        else if (s.StartsWith("00", StringComparison.Ordinal)) digits = s[2..];
        else if (cc == "1" && s.StartsWith("011", StringComparison.Ordinal)) digits = s[3..];
        else if (cc.Length == 0) return null;
        else if (cc == "1") digits = s.Length == 11 && s.StartsWith('1') ? s : "1" + s;
        else digits = cc + (s.StartsWith('0') ? s[1..] : s);
        return Regex.IsMatch(digits, @"\A[1-9][0-9]{7,14}\z") ? "+" + digits : null;
    }

    public static bool IsE164(string value) => Regex.IsMatch(value, @"\A\+[1-9][0-9]{7,14}\z");

    // First 16 bytes of SHA-256("pigeonpost:phone:v1:" + E.164), lowercase hex (§3.1).
    public static string Hash(string e164)
    {
        if (!IsE164(e164)) throw new ArgumentException("Use a phone number with its country code.");
        return Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(HashPrefix + e164)).AsSpan(0, 16));
    }

    private static string DecodeQuotedPrintable(string value)
    {
        var bytes = new List<byte>();
        for (var i = 0; i < value.Length; i++)
        {
            var c = value[i];
            if (c == '=' && i + 2 < value.Length &&Uri.IsHexDigit(value[i + 1]) && Uri.IsHexDigit(value[i + 2]))
            {
                bytes.Add(Convert.ToByte(value.Substring(i + 1, 2), 16));
                i += 2;
            }
            else if (c < 128) bytes.Add((byte)c);
            else bytes.AddRange(Encoding.UTF8.GetBytes(c.ToString()));
        }
        try { return new UTF8Encoding(false, true).GetString(bytes.ToArray()); }
        catch (DecoderFallbackException) { return string.Concat(bytes.Select(b => (char)b)); }
    }

    private static string Unescape(string value) => Regex.Replace(value, @"\\([\\;,nN])", m => m.Groups[1].Value is "n" or "N" ? " " : m.Groups[1].Value);

    private static List<string> SplitUnescaped(string value, char separator)
    {
        var parts = new List<string>(); var current = new StringBuilder();
        for (var i = 0; i < value.Length; i++)
        {
            if (value[i] == '\\' && i + 1 < value.Length) { current.Append(value[i]).Append(value[i + 1]); i++; continue; }
            if (value[i] == separator) { parts.Add(current.ToString()); current.Clear(); continue; }
            current.Append(value[i]);
        }
        parts.Add(current.ToString());
        return parts;
    }

    public static IReadOnlyList<ImportedContact> ParseVcard(string? text)
    {
        var physical = (text ?? "").TrimStart('﻿').Replace("\r\n", "\n").Replace('\r', '\n').Split('\n');
        var lines = new List<string>();
        foreach (var line in physical)
        {
            if (lines.Count > 0 && line.Length > 0 && line[0] is ' ' or '\t') lines[^1] += line[1..];
            else if (lines.Count > 0 && lines[^1].Split(':')[0].Contains("QUOTED-PRINTABLE", StringComparison.OrdinalIgnoreCase) && lines[^1].EndsWith('='))
                lines[^1] = lines[^1][..^1] + line;
            else lines.Add(line);
        }
        var cards = new List<ImportedContact>();
        (string Fn, string N, List<string> Phones)? card = null;
        foreach (var line in lines)
        {
            var colon = line.IndexOf(':');
            if (colon < 0) continue;
            var head = line[..colon]; var rawValue = line[(colon + 1)..];
            var parts = head.Split(';');
            var name = Regex.Replace(parts[0], @"^[^.]*\.", "").Trim().ToUpperInvariant();
            var parameters = parts.Skip(1).Select(p => p.Trim().ToUpperInvariant()).ToArray();
            if (name == "BEGIN" && rawValue.Trim().Equals("VCARD", StringComparison.OrdinalIgnoreCase)) { card = ("", "", new List<string>()); continue; }
            if (card is not { } c) continue;
            if (name == "END" && rawValue.Trim().Equals("VCARD", StringComparison.OrdinalIgnoreCase))
            {
                cards.Add(new((c.Fn.Length > 0 ? c.Fn : c.N).Trim(), c.Phones));
                card = null;
                continue;
            }
            var qp = parameters.Any(p => p is "QUOTED-PRINTABLE" or "ENCODING=QUOTED-PRINTABLE");
            var value = qp ? DecodeQuotedPrintable(rawValue) : rawValue;
            if (name == "FN") card = c with { Fn = Regex.Replace(Unescape(value), @"\s+", " ") };
            else if (name == "N")
            {
                var n = SplitUnescaped(value, ';').Select(Unescape).ToList();
                while (n.Count < 5) n.Add("");
                card = c with { N = string.Join(' ', new[] { n[3], n[1], n[2], n[0], n[4] }.Select(s => s.Trim()).Where(s => s.Length > 0)) };
            }
            else if (name == "TEL" && value.Trim() is { Length: > 0 } number) c.Phones.Add(number);
        }
        return cards;
    }

    private static List<List<string>> CsvRows(string? text)
    {
        var s = (text ?? "").TrimStart('﻿');
        var rows = new List<List<string>>(); var row = new List<string>(); var field = new StringBuilder(); var quoted = false;
        for (var i = 0; i < s.Length; i++)
        {
            var c = s[i];
            if (quoted)
            {
                if (c == '"' && i + 1 < s.Length && s[i + 1] == '"') { field.Append('"'); i++; }
                else if (c == '"') quoted = false;
                else field.Append(c);
            }
            else if (c == '"' && field.Length == 0) quoted = true;
            else if (c == ',') { row.Add(field.ToString()); field.Clear(); }
            else if (c is '\n' or '\r')
            {
                if (c == '\r' && i + 1 < s.Length && s[i + 1] == '\n') i++;
                row.Add(field.ToString()); rows.Add(row); row = new(); field.Clear();
            }
            else field.Append(c);
        }
        if (field.Length > 0 || row.Count > 0) { row.Add(field.ToString()); rows.Add(row); }
        return rows.Where(r => r.Any(f => f.Trim().Length > 0)).ToList();
    }

    // Google Contacts ("Phone 1 - Value", values joined by " ::: "), Outlook ("Mobile Phone", "Business Phone"…) and any
    // header that says phone, mobile, cell or tel, minus the label/type columns beside them.
    public static IReadOnlyList<ImportedContact> ParseCsv(string? text)
    {
        var rows = CsvRows(text);
        if (rows.Count < 2) return [];
        var lower = rows[0].Select(h => h.Trim().ToLowerInvariant()).ToList();
        int Col(params string[] names) => names.Select(n => lower.IndexOf(n)).FirstOrDefault(i => i >= 0, -1);
        var nameCol = Col("name", "display name", "full name", "file as");
        var firstCol = Col("first name", "given name");
        var middleCol = Col("middle name", "additional name");
        var lastCol = Col("last name", "family name", "surname");
        var fallbackCol = Col("nickname", "organization name", "organization 1 - name", "company");
        var phoneCols = lower.Select((h, i) => Regex.IsMatch(h, @"\b(phone|mobile|cell|tel|telephone)\b") && !Regex.IsMatch(h, @"(type|label)\b") ? i : -1)
            .Where(i => i >= 0).ToArray();
        var contacts = new List<ImportedContact>();
        foreach (var r in rows.Skip(1))
        {
            string Get(int i) => i >= 0 && i < r.Count ? r[i].Trim() : "";
            var name = Get(nameCol);
            if (name.Length == 0) name = string.Join(' ', new[] { Get(firstCol), Get(middleCol), Get(lastCol) }.Where(v => v.Length > 0));
            if (name.Length == 0) name = Get(fallbackCol);
            var phones = phoneCols.SelectMany(i => Get(i).Split(":::")).Select(p => p.Trim()).Where(p => p.Length > 0).ToList();
            contacts.Add(new(Regex.Replace(name, @"\s+", " "), phones));
        }
        return contacts;
    }

    // Reads a .vcf or .csv export, normalises every number and hashes it. Each number is listed once (under the first
    // name it was filed under); numbers that cannot be read are counted, never sent.
    public static ContactImportResult Read(string fileName, string text, string? callingCode)
    {
        var vcard = fileName.EndsWith(".vcf", StringComparison.OrdinalIgnoreCase) || fileName.EndsWith(".vcard", StringComparison.OrdinalIgnoreCase)
            || text.Contains("BEGIN:VCARD", StringComparison.OrdinalIgnoreCase);
        var contacts = vcard ? ParseVcard(text) : ParseCsv(text);
        var seen = new HashSet<string>(StringComparer.Ordinal); var numbers = new List<ImportedNumber>(); var unreadable = 0;
        foreach (var contact in contacts)
            foreach (var raw in contact.Phones)
            {
                if (NormalizeE164(raw, callingCode) is not { } phone) { unreadable++; continue; }
                if (seen.Add(phone)) numbers.Add(new(contact.Name.Length > 0 ? contact.Name : phone, phone, Hash(phone)));
            }
        return new(numbers, unreadable, contacts.Count);
    }

    // The sender's name as the invitation SMS carries it (§5): letters, digits, spaces and .'- only, at most 24
    // characters, otherwise "A friend".
    public static string InviteName(string? displayName)
    {
        var kept = Regex.Replace(Regex.Replace(displayName ?? "", @"[^\p{L}\p{N} .'-]", ""), @"\s+", " ").Trim();
        var runes = kept.EnumerateRunes().Take(24).Select(r => r.ToString());
        var cut = string.Concat(runes).Trim();
        return cut.Length > 0 ? cut : "A friend";
    }

    public static string InviteText(string? displayName) =>
        "Pigeonpost: " + InviteName(displayName) + " invited you to chat on Pigeonpost. Join: https://pigeonpost.dev/invite?c=… Reply STOP to opt out.";

    // Common countries first; anything else is typed as "+code" (same list as the web).
    public static IReadOnlyList<(string Region, string Name, string Code)> Countries { get; } =
    [
        ("TR", "Türkiye", "90"), ("US", "United States / Canada", "1"), ("GB", "United Kingdom", "44"), ("DE", "Germany", "49"),
        ("FR", "France", "33"), ("NL", "Netherlands", "31"), ("ES", "Spain", "34"), ("IT", "Italy", "39"), ("AU", "Australia", "61"),
        ("IN", "India", "91"),
    ];

    // A starting guess from the Windows home region (§2). Only a default; the person picks.
    public static string GuessCallingCode(string? region)
    {
        var r = (region ?? "").ToUpperInvariant();
        if (r == "CA") return "1";
        return Countries.FirstOrDefault(c => c.Region == r).Code ?? "1";
    }
}
