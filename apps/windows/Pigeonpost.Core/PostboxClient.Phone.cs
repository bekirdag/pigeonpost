using System.Text.Json.Serialization;
using System.Text.RegularExpressions;

namespace Pigeonpost.Core;

// docs/contacts-and-phone.md §3–§5. The full number is never returned; `Masked` is all the client may show.
public sealed record VerifiedPhone(string Id, string Identity, string CountryCode, string Masked, bool Discoverable, long VerifiedAt)
{
    public override string ToString() => "VerifiedPhone(redacted)";
}
public sealed record PhoneChallenge([property: JsonPropertyName("challenge_id")] string Id, long ExpiresAt, long ResendAfter, int CodeLength);
public sealed record DiscoveryMatch(string Hash, string Address, string? Handle, string? DisplayName);
public sealed record DiscoveryResult(IReadOnlyList<DiscoveryMatch> Matches, int Checked,
    [property: JsonPropertyName("new")] int Fresh, [property: JsonPropertyName("remaining_today")] int Remaining);
public sealed record InvitationResult(string Status, string? Reason, string? InviteId);

public sealed partial class PostboxClient
{
    private sealed record PhonesResponse(IReadOnlyList<VerifiedPhone>? Numbers, int MaxNumbers);
    private sealed record PhoneResponse(VerifiedPhone? Number);

    private static bool PeopleId(string value) => Regex.IsMatch(value, @"\A[A-Za-z0-9_-]{1,128}\z");
    private static VerifiedPhone CheckPhone(VerifiedPhone? phone)
    {
        if (phone is null || !PeopleId(phone.Id) || !PeopleNames.Mailbox(phone.Identity) || !Regex.IsMatch(phone.CountryCode ?? "", @"\A[0-9]{1,3}\z")
            || phone.Masked is not { Length: >= 1 and <= 64 } masked || masked.Any(char.IsControl) || phone.VerifiedAt <= 0)
            throw new PostboxException(200, "bad_response");
        return phone;
    }

    public async Task<IReadOnlyList<VerifiedPhone>> GetPhonesAsync(string mailbox, CancellationToken ct)
    {
        var response = await PeopleRequest<PhonesResponse>(HttpMethod.Get, "/v1/phone", mailbox, null, ct).ConfigureAwait(false);
        var numbers = response.Numbers ?? throw new PostboxException(200, "bad_response");
        if (response.MaxNumbers is < 1 or > 3 || numbers.Count > response.MaxNumbers || numbers.Select(n => n?.Id).Distinct().Count() != numbers.Count)
            throw new PostboxException(200, "bad_response");
        return numbers.Select(CheckPhone).ToArray();
    }

    public async Task<PhoneChallenge> SendPhoneCodeAsync(string mailbox, string phone, CancellationToken ct)
    {
        if (!ContactImport.IsE164(phone)) throw new ArgumentException("Use a phone number with its country code.");
        var challenge = await PeopleRequest<PhoneChallenge>(HttpMethod.Post, "/v1/phone/verify", mailbox, new { phone, identity = mailbox }, ct).ConfigureAwait(false);
        if (challenge is null || !PeopleId(challenge.Id ?? "") || challenge.ExpiresAt <= 0 || challenge.ResendAfter < 0 || challenge.CodeLength != 6)
            throw new PostboxException(200, "bad_response");
        return challenge;
    }

    public async Task<VerifiedPhone> VerifyPhoneAsync(string mailbox, string challenge, string code, bool discoverable, CancellationToken ct)
    {
        if (!PeopleId(challenge)) throw new ArgumentException("Send a new code.");
        if (!Regex.IsMatch(code, @"\A[0-9]{6}\z")) throw new ArgumentException("Enter the 6-digit code.");
        var response = await PeopleRequest<PhoneResponse>(HttpMethod.Post, "/v1/phone/verify/" + challenge, mailbox,
            new { code, discoverable }, ct).ConfigureAwait(false);
        var phone = CheckPhone(response.Number);
        if (phone.Identity != mailbox) throw new PostboxException(200, "bad_response");
        return phone;
    }

    public async Task<VerifiedPhone> SetPhoneDiscoverableAsync(string mailbox, string id, bool discoverable, CancellationToken ct)
    {
        if (!PeopleId(id)) throw new ArgumentException("Choose a number.");
        var response = await PeopleRequest<PhoneResponse>(HttpMethod.Patch, "/v1/phone/" + id, mailbox, new { discoverable }, ct).ConfigureAwait(false);
        var phone = CheckPhone(response.Number);
        if (phone.Id != id || phone.Discoverable != discoverable) throw new PostboxException(200, "bad_response");
        return phone;
    }

    public async Task UnlinkPhoneAsync(string mailbox, string id, CancellationToken ct)
    {
        if (!PeopleId(id)) throw new ArgumentException("Choose a number.");
        _ = await PeopleRequest<Dictionary<string, bool>>(HttpMethod.Delete, "/v1/phone/" + id, mailbox, null, ct).ConfigureAwait(false);
    }

    // At most 200 hashes per call (the web's batch size, under the server's 500), never names or raw numbers.
    public async Task<DiscoveryResult> DiscoverContactsAsync(string mailbox, IReadOnlyList<string> hashes, CancellationToken ct)
    {
        if (hashes.Count is < 1 or > 200 || hashes.Distinct(StringComparer.Ordinal).Count() != hashes.Count
            || hashes.Any(h => !Regex.IsMatch(h, @"\A[a-f0-9]{32}\z")))
            throw new ArgumentException("Choose up to 200 different numbers.");
        var result = await PeopleRequest<DiscoveryResult>(HttpMethod.Post, "/v1/contacts/discover", mailbox, new { hashes }, ct).ConfigureAwait(false);
        var matches = result?.Matches ?? throw new PostboxException(200, "bad_response");
        if (result.Checked != hashes.Count || result.Fresh < 0 || result.Fresh > result.Checked || result.Remaining is < 0 or > 1000
            || matches.Count > hashes.Count || matches.Select(m => m?.Hash).Distinct().Count() != matches.Count)
            throw new PostboxException(200, "bad_response");
        foreach (var match in matches)
        {
            try
            {
                if (match is null || !hashes.Contains(match.Hash) || !PeopleNames.Mailbox(match.Address)
                    || match.Handle is { } handle && (!PostAddress.IsValid(handle) || handle.StartsWith("/k/", StringComparison.Ordinal))
                    || match.DisplayName is { } name && PeopleNames.Normalize(name) != name) throw new ArgumentException();
            }
            catch (ArgumentException) { throw new PostboxException(200, "bad_response"); }
        }
        return result;
    }

    // `confirm: true` records that a person pressed Invite on this one number (§5); callers must only use it then.
    public async Task<InvitationResult> InviteBySmsAsync(string mailbox, string phone, CancellationToken ct)
    {
        if (!ContactImport.IsE164(phone)) throw new ArgumentException("Use a phone number with its country code.");
        var result = await PeopleRequest<InvitationResult>(HttpMethod.Post, "/v1/contacts/invites", mailbox, new { phone, confirm = true }, ct).ConfigureAwait(false);
        if (result is null || !(result.Status == "sent" && result.InviteId is { } id && PeopleId(id)
            || result.Status == "not_sent" && result.Reason is "already_invited" or "unavailable"))
            throw new PostboxException(200, "bad_response");
        return result;
    }

    // Words for the §3–§5 refusals; anything else keeps PostboxException's own text.
    public static string PeopleRefusal(Exception error) => error switch
    {
        ArgumentException a => a.Message,
        PostboxException { Code: "rate_limited" } p => p.RetryAfter is { } wait
            ? $"Too many tries. Please wait {(wait >= 120 ? $"{wait / 60} minutes" : $"{wait} seconds")} and try again." : "Too many tries. Please try again later.",
        PostboxException { Code: "discovery_locked" } => "Finding people is paused for this account because of unusual use. Please try again later.",
        PostboxException { Code: "discovery_busy" } => "Finding people is busy right now. Please try again in a few minutes.",
        PostboxException { Code: "sms_unavailable" } => "Text messages can't be sent right now. Please try again later.",
        PostboxException { Code: "phone_required" } => "Verify your own phone number first, in Phone number.",
        PostboxException { Code: "phone_not_supported" } => "This number can't receive Pigeonpost codes.",
        PostboxException { Code: "invalid_code" } p => p.DetailNumber("attempts_left") is { } left
            ? $"That code is not right. {left} {(left == 1 ? "try" : "tries")} left." : "That code is not right.",
        PostboxException { Code: "challenge_expired" } => "This code expired. Send a new one.",
        PostboxException { Code: "credential_not_accepted" or "human_required" } => "Only you can do this, signed in on this device.",
        PostboxException { Code: "feature_disabled" } => "This isn't available for your account yet.",
        PostboxException p => p.Message,
        _ => "Could not reach the postbox. Please try again.",
    };
}
