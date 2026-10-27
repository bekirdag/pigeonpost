using System.Security.Cryptography;
using System.Text;
using System.Text.Json;

namespace Pigeonpost.Core;

public enum StrongSignInProblem { Unverified, DifferentAccount }
public sealed class StrongSignInException(StrongSignInProblem problem = StrongSignInProblem.Unverified) : Exception(
    problem == StrongSignInProblem.DifferentAccount ? "Use the account already signed in to Pigeonpost."
        : "The fresh sign-in could not be verified. Try again.")
{ public StrongSignInProblem Problem { get; } = problem; }

// Only for responses from the app's fixed trusted HTTPS exchange, never uploaded/pasted tokens.
// This checks claim bindings, not JWT signatures; the postbox independently verifies its bearer.
public static class StrongSignInProof
{
    public static string? SubjectFromTrustedExchange(string? token, string issuer, string client)
    {
        try
        {
            var claims = Parsed(token).Claims;
            var subject = Text(claims,"sub");
            return Text(claims,"iss") == issuer && Text(claims,"azp") == client && SafeSubject(subject) ? subject : null;
        }
        catch (StrongSignInException) { return null; }
    }

    public static void ValidateTrustedExchange(string? idToken, string accessToken, string subject, string nonce,
        StrongSignInRequirement requirement, string issuer, string client, long startedAt, long now)
    {
        if (!SafeSubject(subject) || nonce.Length is < 1 or > 128 || nonce.Any(char.IsControl)
            || startedAt is < 0 or > 253402300799L || now is < 0 or > 253402300799L || startedAt > now + 30)
            throw new StrongSignInException();
        var id = Parsed(idToken); var access = Parsed(accessToken);
        if (Text(id.Claims,"sub") != subject || Text(access.Claims,"sub") != subject)
            throw new StrongSignInException(StrongSignInProblem.DifferentAccount);
        var audience = id.Claims.TryGetProperty("aud",out var aud) ? aud : default;
        var correctAudience = audience.ValueKind == JsonValueKind.String ? audience.GetString() == client
            : audience.ValueKind == JsonValueKind.Array && audience.GetArrayLength() == 1
                && audience[0].ValueKind == JsonValueKind.String && audience[0].GetString() == client;
        if (!correctAudience || Text(id.Claims,"iss") != issuer || Text(access.Claims,"iss") != issuer
            || Text(access.Claims,"azp") != client || Text(id.Claims,"nonce") != nonce
            || id.Claims.TryGetProperty("azp",out var azp) && (azp.ValueKind != JsonValueKind.String || azp.GetString() != client))
            throw new StrongSignInException();
        foreach (var token in new[]{id,access})
        {
            var expires = Number(token.Claims,"exp"); var issued = Number(token.Claims,"iat"); var authenticated = Number(token.Claims,"auth_time");
            if (expires <= now || expires <= issued || issued < startedAt - 30 || issued > now + 30
                || authenticated > now + 30 || authenticated < now - requirement.MaxAge - 30
                || requirement.MaxAge == 0 && authenticated < startedAt - 5
                || !requirement.AcrValues.Contains(Text(token.Claims,"acr"),StringComparer.Ordinal)) throw new StrongSignInException();
        }
        if (id.Claims.TryGetProperty("at_hash",out var hash))
        {
            if (hash.ValueKind != JsonValueKind.String) throw new StrongSignInException();
            var digest = SHA256.HashData(Encoding.UTF8.GetBytes(accessToken));
            var expected = Convert.ToBase64String(digest,0,16).TrimEnd('=').Replace('+','-').Replace('/','_');
            if (!CryptographicOperations.FixedTimeEquals(Encoding.UTF8.GetBytes(hash.GetString()!),Encoding.UTF8.GetBytes(expected)))
                throw new StrongSignInException();
        }
    }

    private sealed record Token(JsonElement Claims);
    private static Token Parsed(string? raw)
    {
        if (raw is null || raw.Length > 32768) throw new StrongSignInException();
        var parts = raw.Split('.');
        if (parts.Length != 3 || parts.Any(p => p.Length == 0 || p.Any(c => !char.IsAsciiLetterOrDigit(c) && c is not ('_' or '-'))))
            throw new StrongSignInException();
        try
        {
            using var header = Decode(parts[0]); using var claims = Decode(parts[1]);
            if (!UniqueObject(header.RootElement) || !UniqueObject(claims.RootElement) || Text(header.RootElement,"alg") != "RS256")
                throw new StrongSignInException();
            return new(claims.RootElement.Clone());
        }
        catch (Exception error) when (error is JsonException or FormatException or InvalidOperationException or ArgumentException)
        { throw new StrongSignInException(); }
    }
    private static JsonDocument Decode(string value) => JsonDocument.Parse(Convert.FromBase64String(
        value.Replace('-','+').Replace('_','/') + new string('=',(4-value.Length%4)%4)));
    private static bool UniqueObject(JsonElement value) => value.ValueKind == JsonValueKind.Object
        && value.EnumerateObject().Select(p=>p.Name).Distinct(StringComparer.Ordinal).Count() == value.EnumerateObject().Count();
    private static string Text(JsonElement body,string name) => body.TryGetProperty(name,out var value) && value.ValueKind == JsonValueKind.String
        ? value.GetString()! : throw new StrongSignInException();
    private static long Number(JsonElement body,string name) => body.TryGetProperty(name,out var value) && value.ValueKind == JsonValueKind.Number
        && value.TryGetInt64(out var number) && number is >= 0 and <= 253402300799L ? number : throw new StrongSignInException();
    private static bool SafeSubject(string value) => value.Length > 0 && Encoding.UTF8.GetByteCount(value) <= 256 && !value.Any(char.IsControl);
}
