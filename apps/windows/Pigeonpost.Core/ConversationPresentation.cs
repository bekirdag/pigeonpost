using System.Globalization;

namespace Pigeonpost.Core;

// Display-only helpers. The full routing address and server permission fields stay unchanged.
public static class ConversationPresentation
{
    public static string Initials(string name)
    {
        if (string.IsNullOrWhiteSpace(name) || name.StartsWith("/k/", StringComparison.Ordinal)) return "?";
        var words = name.Split([' ', '/', '-', '_'], StringSplitOptions.RemoveEmptyEntries);
        if (words.Length == 0) return "?";
        var first = StringInfo.GetNextTextElement(words[0]);
        var last = words.Length > 1 ? StringInfo.GetNextTextElement(words[^1]) : "";
        return (first + last).ToUpperInvariant();
    }

    public static string ActivityLabel(long timestamp, DateTimeOffset now)
    {
        if (timestamp <= 0 || timestamp > 253402300799) return "";
        var date = DateTimeOffset.FromUnixTimeSeconds(timestamp).ToLocalTime();
        var today = now.ToLocalTime().Date;
        if (date.Date == today) return date.ToString("t");
        if (date.Date == today.AddDays(-1)) return "Yesterday";
        return date.ToString(date.Year == today.Year ? "MMM d" : "MMM d, yyyy");
    }
}
