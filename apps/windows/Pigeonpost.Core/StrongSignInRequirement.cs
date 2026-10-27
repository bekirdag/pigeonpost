using System.Text;
using System.Text.Json;

namespace Pigeonpost.Core;

// Challenge data can request authentication strength/recency, never an issuer or redirect.
public sealed class StrongSignInRequirement
{
    private StrongSignInRequirement(string[] methods, int age)
    { AcrValues = Array.AsReadOnly(methods); MaxAge = age; }
    public IReadOnlyList<string> AcrValues { get; }
    public string RequestedAcr => string.Join(' ', AcrValues);
    public int MaxAge { get; }
    public override string ToString() => $"StrongSignInRequirement(methods={AcrValues.Count}, maxAge={MaxAge})";

    public static StrongSignInRequirement? Create(IEnumerable<string> methods, int maxAge)
    {
        var values = methods.Take(9).ToArray();
        if (maxAge is < 0 or > 600 || values.Length is < 1 or > 8
            || values.Distinct(StringComparer.Ordinal).Count() != values.Length
            || values.Any(v => v is null || v.Length is < 1 or > 128
                || v.Any(c => !char.IsAsciiLetterOrDigit(c) && !":/._-".Contains(c)))) return null;
        return new(values, maxAge);
    }

    internal static bool SignalsChallenge(string? header) => header is not null
        && (header.Length > 2048 || header.Contains("insufficient_user_authentication", StringComparison.OrdinalIgnoreCase));

    public static StrongSignInRequirement? Parse(JsonElement? problem, string? header)
    {
        var parameters = BearerParameters(header);
        if (parameters is not null && parameters.TryGetValue("max_age", out var age)
            && age.Length is >= 1 and <= 3 && age.All(char.IsAsciiDigit) && int.TryParse(age, out var number)
            && parameters.TryGetValue("acr_values", out var acr)
            && Create(acr.Split(' ', StringSplitOptions.RemoveEmptyEntries), number) is { } valid) return valid;
        if (problem is not { ValueKind: JsonValueKind.Object } body
            || body.EnumerateObject().Select(p => p.Name).Distinct(StringComparer.Ordinal).Count() != body.EnumerateObject().Count()
            || !body.TryGetProperty("max_age", out var bodyAge) || bodyAge.ValueKind != JsonValueKind.Number || !bodyAge.TryGetInt32(out var seconds)
            || !body.TryGetProperty("acr_values", out var raw)) return null;
        if (raw.ValueKind == JsonValueKind.String)
            return Create(raw.GetString()!.Split(' ', StringSplitOptions.RemoveEmptyEntries), seconds);
        if (raw.ValueKind != JsonValueKind.Array || raw.GetArrayLength() > 8) return null;
        var values = new List<string>();
        foreach (var item in raw.EnumerateArray())
        {
            if (item.ValueKind != JsonValueKind.String) return null;
            values.Add(item.GetString()!);
        }
        return Create(values, seconds);
    }

    // Split only outside quoted strings; descriptions cannot introduce another parameter.
    private static Dictionary<string, string>? BearerParameters(string? header)
    {
        if (header is null || header.Length > 2048 || !header.StartsWith("Bearer ", StringComparison.OrdinalIgnoreCase)
            || header.Any(c => c < 32 || c == 127)) return null;
        var fields = header[7..]; var index = 0;
        var result = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        while (index < fields.Length)
        {
            while (index < fields.Length && fields[index] == ' ') index++;
            var start = index;
            while (index < fields.Length && (char.IsAsciiLetterOrDigit(fields[index]) || "_-".Contains(fields[index]))) index++;
            if (index == start) return null;
            var name = fields[start..index];
            while (index < fields.Length && fields[index] == ' ') index++;
            if (index >= fields.Length || fields[index++] != '=') return null;
            while (index < fields.Length && fields[index] == ' ') index++;
            if (index >= fields.Length) return null;
            var value = new StringBuilder();
            if (fields[index] == '"')
            {
                index++; var closed = false;
                while (index < fields.Length)
                {
                    var character = fields[index++];
                    if (character == '"') { closed = true; break; }
                    if (character == '\\')
                    {
                        if (index >= fields.Length || fields[index] is not ('\\' or '"')) return null;
                        value.Append(fields[index++]);
                    }
                    else value.Append(character);
                }
                if (!closed) return null;
            }
            else
            {
                var begin = index;
                while (index < fields.Length && fields[index] is not (',' or ' '))
                {
                    if (fields[index] is '"' or '\\' || fields[index] > 126) return null;
                    value.Append(fields[index++]);
                }
                if (index == begin) return null;
            }
            if (!result.TryAdd(name, value.ToString())) return null;
            while (index < fields.Length && fields[index] == ' ') index++;
            if (index == fields.Length) return result;
            if (fields[index++] != ',' || index == fields.Length) return null;
        }
        return null;
    }
}
