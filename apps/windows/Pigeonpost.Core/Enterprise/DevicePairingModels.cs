using System.Text;

namespace Pigeonpost.Core.Enterprise;

// Terminal proof: view memory only, never a URL query, persistent preference or diagnostic.
public sealed class DevicePairingCode : IEquatable<DevicePairingCode>
{
    private readonly string value;
    private DevicePairingCode(string value) => this.value = value;
    public string DisplayValue => value[..4] + "-" + value[4..];
    public static DevicePairingCode? Parse(string input)
    {
        if (input.Length > 128 || input.Any(c => c > 127)) return null;
        var code = input.Trim(' ', '\t', '\r', '\n').ToUpperInvariant();
        if (code.Length == 9 && code[4] == '-') code = code.Remove(4, 1);
        return code.Length == 8 && code.All(c => char.IsAsciiLetterOrDigit(c)) ? new(code) : null;
    }
    public bool Equals(DevicePairingCode? other) => other is not null && value == other.value;
    public override bool Equals(object? other) => other is DevicePairingCode code && Equals(code);
    public override int GetHashCode() => StringComparer.Ordinal.GetHashCode(value);
    public override string ToString() => "DevicePairingCode(redacted)";
}

public static class DevicePairingLink
{
    public static DevicePairingCode? Parse(string input)
    {
        const string origin = "https://inbox.pigeonpost.dev";
        const string path = "/pair.html#";
        if (input.Length > 4096 || input.Any(c => c < 32 || c > 126)
            || !input.StartsWith(origin, StringComparison.OrdinalIgnoreCase)
            || !input.AsSpan(origin.Length).StartsWith(path, StringComparison.Ordinal)) return null;
        var fragment = input[(origin.Length + path.Length)..];
        if (fragment != fragment.Trim()) return null;
        return DevicePairingCode.Parse(fragment);
    }
}

public sealed record PairingOrganization(string Id, string Name);

// A server projection, never a local grant. Missing/unknown authority disables approval.
public sealed record DevicePairingPreview(string PairingId, PairingOrganization Org, string Handle,
    string Confirmation, IReadOnlyList<string> Capabilities, long RequestedAt, long ExpiresAt,
    string? Address = null, bool? CreatesAgent = null, string? Label = null, string? IpCountry = null,
    bool CanApprove = false, string? Reason = null, string? State = null)
{
    public string ConfirmationDigits => Confirmation?.Replace(" ", "") ?? "";
    public string DisplayConfirmation => ConfirmationDigits.Length == 6 ? ConfirmationDigits[..3] + " " + ConfirmationDigits[3..] : "";
    public bool IsValid => SafeId(PairingId) && Org is not null && SafeId(Org.Id) && SafeText(Org.Name, 320)
        && Handle is { Length: > 0 and <= 160 } && Handle.Count(c => c == '/') == 2
        && !Handle.StartsWith("/k/", StringComparison.Ordinal) && PostAddress.IsValid(Handle)
        && (Address is not null ? MessageRecoveryScope.CanonicalMailbox(Address) && CreatesAgent != true : CreatesAgent == true)
        && (Label is null || SafeText(Label, 320))
        && Confirmation is not null && (Confirmation.Length == 6 || Confirmation.Length == 7 && Confirmation[3] == ' ')
        && ConfirmationDigits.Length == 6 && ConfirmationDigits.All(char.IsAsciiDigit)
        && Capabilities is { Count: > 0 and <= 32 } && Capabilities.Distinct(StringComparer.Ordinal).Count() == Capabilities.Count
        && Capabilities.All(SafeId) && (IpCountry is null || SafeText(IpCountry, 80)) && (Reason is null || SafeText(Reason, 160))
        && RequestedAt >= 0 && ExpiresAt > RequestedAt && ExpiresAt <= 253402300799L
        && State is null or "pending" or "approved" or "denied";
    public bool AllowsDecision(long now) => IsValid && (State is null or "pending") && ExpiresAt > now && RequestedAt <= now + 60;
    public bool AllowsApproval(long now) => AllowsDecision(now) && CanApprove && Capabilities.Count == 1 && Capabilities[0] == "mailbox";
    internal static bool SafeId(string? value) => value is { Length: > 0 and <= 128 }
        && value.All(c => char.IsAsciiLetterOrDigit(c) || c is '_' or '-');
    private static bool SafeText(string? value, int maxBytes) => !string.IsNullOrEmpty(value)
        && Encoding.UTF8.GetByteCount(value) <= maxBytes && value.All(c => !char.IsControl(c)
            && c is not (>= '\u202a' and <= '\u202e') and not (>= '\u2066' and <= '\u2069') and not ('\u200e' or '\u200f' or '\u061c'));
    public override string ToString() => "DevicePairingPreview(redacted)";
}
