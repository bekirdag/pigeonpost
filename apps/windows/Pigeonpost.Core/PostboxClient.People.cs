using System.Text.Json;

namespace Pigeonpost.Core;

public sealed partial class PostboxClient
{
    private sealed record NameResponse(ProfileName? Name);
    private sealed record ProfilesResponse(Dictionary<string, PeerProfile?>? Profiles);
    private async Task<T> PeopleRequest<T>(HttpMethod method, string path, string mailbox, object? body, CancellationToken ct)
    {
        if (!PeopleNames.Mailbox(mailbox)) throw new ArgumentException("Choose an inbox first.");
        using var response = await RequestAsync(method, Scoped(path, mailbox),
            body is null ? null : JsonSerializer.Serialize(body, PostboxJson.Options), ct, identity: mailbox).ConfigureAwait(false);
        await response.Content.LoadIntoBufferAsync(512 * 1024, ct).ConfigureAwait(false);
        return await DecodeAsync<T>(response, ct).ConfigureAwait(false);
    }
    private static ProfileName CheckName(ProfileName name)
    {
        try
        {
            if (!PeopleNames.Visibility(name.Visibility) || name.UpdatedAt <= 0 || PeopleNames.Normalize(name.DisplayName) != name.DisplayName)
                throw new ArgumentException();
            return name;
        }
        catch (ArgumentException) { throw new PostboxException(200, "bad_response"); }
    }
    public async Task<ProfileName?> GetProfileNameAsync(string mailbox, CancellationToken ct)
    {
        var response = await PeopleRequest<NameResponse>(HttpMethod.Get, "/v1/profile/name", mailbox, null, ct).ConfigureAwait(false);
        return response.Name is { } name ? CheckName(name) : null;
    }
    public async Task<ProfileName> SetProfileNameAsync(string mailbox, string name, string visibility, CancellationToken ct)
    {
        if (!PeopleNames.Visibility(visibility)) throw new ArgumentException("Choose who can see your name.");
        var normalized = PeopleNames.Normalize(name);
        var response = await PeopleRequest<NameResponse>(HttpMethod.Put, "/v1/profile/name", mailbox,
            new { display_name = normalized, visibility }, ct).ConfigureAwait(false);
        var saved = CheckName(response.Name ?? throw new PostboxException(200, "bad_response"));
        if (saved.DisplayName != normalized || saved.Visibility != visibility) throw new PostboxException(200, "bad_response");
        return saved;
    }
    public async Task RemoveProfileNameAsync(string mailbox, CancellationToken ct) =>
        _ = await PeopleRequest<Dictionary<string, bool>>(HttpMethod.Delete, "/v1/profile/name", mailbox, null, ct).ConfigureAwait(false);
    public async Task<IReadOnlyDictionary<string, PeerProfile?>> GetPeerProfilesAsync(string mailbox, IReadOnlyList<string> addresses, CancellationToken ct)
    {
        if (addresses.Count is < 1 or > 100 || addresses.Any(a => !PostAddress.IsValid(a)) || addresses.Distinct().Count() != addresses.Count)
            throw new ArgumentException("Choose up to 100 different addresses.");
        var response = await PeopleRequest<ProfilesResponse>(HttpMethod.Get,
            "/v1/profiles?addresses=" + Uri.EscapeDataString(string.Join(',', addresses)), mailbox, null, ct).ConfigureAwait(false);
        var profiles = response.Profiles ?? throw new PostboxException(200, "bad_response");
        foreach (var (key, profile) in profiles)
        {
            if (!addresses.Contains(key)) throw new PostboxException(200, "bad_response");
            if (profile is null) continue;
            try
            {
                if (!PeopleNames.Mailbox(profile.Address) || profile.Handle is { } handle && (!PostAddress.IsValid(handle) || handle.StartsWith("/k/"))
                    || key != profile.Address && key != profile.Handle
                    || profile.DisplayName is { } name && PeopleNames.Normalize(name) != name) throw new ArgumentException();
            }
            catch (ArgumentException) { throw new PostboxException(200, "bad_response"); }
        }
        return profiles;
    }
}
