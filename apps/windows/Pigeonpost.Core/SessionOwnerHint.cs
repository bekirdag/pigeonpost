using System.Security.Cryptography;
using System.Text;
using System.Text.Json;

namespace Pigeonpost.Core;

// Identifies only a token returned by AccountSession's fixed trusted exchange. This is not a
// standalone JWT signature verifier, an MFA proof, or permission to read a mailbox or recovery file.
internal sealed class SessionOwnerHint
{
    internal string StorageKey { get; }
    private SessionOwnerHint(string issuer, string client, string subject) => StorageKey =
        Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes("pigeonpost-message-owner-v1\0" + issuer + "\0" + client + "\0" + subject))).ToLowerInvariant();

    internal static SessionOwnerHint? FromTrustedExchange(string token, Uri issuer, string client)
    {
        if (token.Length > 32768) return null;
        var parts = token.Split('.');
        if (parts.Length != 3 || parts.Any(p => p.Length == 0 || p.Any(c => !char.IsAsciiLetterOrDigit(c) && c is not '_' and not '-'))) return null;
        try
        {
            using var header = Parse(parts[0]); using var claims = Parse(parts[1]);
            if (!UniqueObject(header.RootElement) || !UniqueObject(claims.RootElement)) return null;
            var algorithm = Text(header.RootElement, "alg");
            var expectedIssuer = issuer.AbsoluteUri.TrimEnd('/');
            var subject = Text(claims.RootElement, "sub");
            if (string.IsNullOrEmpty(algorithm) || algorithm.Equals("none", StringComparison.OrdinalIgnoreCase)
                || Text(claims.RootElement, "iss") != expectedIssuer || Text(claims.RootElement, "azp") != client
                || string.IsNullOrEmpty(subject) || Encoding.UTF8.GetByteCount(subject) > 256 || subject.Any(char.IsControl)) return null;
            return new(expectedIssuer, client, subject);
        }
        catch (Exception ex) when (ex is JsonException or FormatException or InvalidOperationException or ArgumentException) { return null; }
    }
    private static JsonDocument Parse(string encoded) => JsonDocument.Parse(Convert.FromBase64String(
        encoded.Replace('-', '+').Replace('_', '/') + new string('=', (4 - encoded.Length % 4) % 4)));
    private static bool UniqueObject(JsonElement value) => value.ValueKind == JsonValueKind.Object
        && value.EnumerateObject().Select(p => p.Name).Distinct(StringComparer.Ordinal).Count() == value.EnumerateObject().Count();
    private static string? Text(JsonElement root, string name) => root.TryGetProperty(name, out var value)
        && value.ValueKind == JsonValueKind.String ? value.GetString() : null;
    public override string ToString() => "SessionOwnerHint(redacted)";
}
