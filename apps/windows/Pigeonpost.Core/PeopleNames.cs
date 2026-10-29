using System.Globalization;
using System.Text;
using System.Text.RegularExpressions;

namespace Pigeonpost.Core;

public sealed record ProfileName(string DisplayName, string Visibility, long UpdatedAt);
public sealed record PeerProfile(string Address, string? Handle, string? DisplayName);
public sealed record PeerName(string? LocalAlias = null, string? DisplayName = null)
{
    public string Resolve(string fallback, string? trustedLabel = null) => LocalAlias is { Length: > 0 } alias ? alias
        : !string.IsNullOrWhiteSpace(trustedLabel) ? trustedLabel
        : DisplayName is { Length: > 0 } name ? "~" + name : fallback;
}

public static class PeopleNames
{
    public static bool Visibility(string value) => value is "known" or "everyone" or "nobody";
    public static bool Mailbox(string value) => value is { Length: > 3 and <= 512 } && Regex.IsMatch(value, @"\A/k/[a-z0-9]+\z", RegexOptions.CultureInvariant);
    public static string Normalize(string input, bool alias = false)
    {
        if (string.IsNullOrEmpty(input) || input.Length > 512) throw new ArgumentException("Use 1–40 characters for your name, or up to 64 for a local name.");
        // Reject malformed UTF-16 before EnumerateRunes can replace it with U+FFFD.
        _ = new UTF8Encoding(false, true).GetByteCount(input);
        foreach (var rune in input.EnumerateRunes())
        {
            var v = rune.Value;
            if (v is '/' or '@' or 0x034f or 0x115f or 0x1160 or 0x17b4 or 0x17b5 or 0x2800 or 0x3164 or 0xffa0
                or >= 0x180b and <= 0x180f or >= 0xfe00 and <= 0xfe0f or >= 0xfff0 and <= 0xfffb or >= 0xe0000 and <= 0xe007f
                || Rune.GetUnicodeCategory(rune) is UnicodeCategory.Control or UnicodeCategory.Format or UnicodeCategory.Surrogate
                    or UnicodeCategory.LineSeparator or UnicodeCategory.ParagraphSeparator)
                throw new ArgumentException("Use a name without addresses or invisible characters.");
        }
        var name = Regex.Replace(input.Normalize(NormalizationForm.FormC).Trim(), @"\s+", " ");
        if (name.EnumerateRunes().Count() < 1 || name.EnumerateRunes().Count() > (alias ? 64 : 40))
            throw new ArgumentException(alias ? "Use 1–64 characters." : "Use 1–40 characters.");
        var plain = string.Concat(name.EnumerateRunes().Where(Rune.IsLetterOrDigit).Select(r => r.ToString())).ToLowerInvariant();
        if (plain.Length == 0) throw new ArgumentException("Include at least one letter or digit.");
        if (!alias && (plain.Contains("pigeonpost", StringComparison.Ordinal)
            || plain is "admin" or "support" or "official" or "verified" or "security" or "system" or "moderator"))
            throw new ArgumentException("Choose a personal name.");
        return name;
    }
}
