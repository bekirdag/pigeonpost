using System.Globalization;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Pigeonpost.Core;

// Phase 1b: filtering, blocking, call history and profile pictures (SDS §12, §13, §15, §16).
// The postbox decides everything; these are its records and the app's wording for them.

public sealed record InboxPolicy
{
    public string? UnknownCalls { get; init; }
    public string? UnnamedCalls { get; init; }
    public string? UnnamedMessages { get; init; }
    public string? UnknownMessages { get; init; }
    public string? LowReputation { get; init; }
    public bool? GroupMembersKnown { get; init; }
    public int? CallHistoryDays { get; init; }
    public bool? ReadReceipts { get; init; }
    [JsonExtensionData] public Dictionary<string, JsonElement>? Other { get; init; }

    public string? Value(string field) => field switch
    {
        "unknown_calls" => UnknownCalls,
        "unnamed_calls" => UnnamedCalls,
        "unnamed_messages" => UnnamedMessages,
        "unknown_messages" => UnknownMessages,
        "low_reputation" => LowReputation,
        "call_history_days" => CallHistoryDays?.ToString(CultureInfo.InvariantCulture),
        "read_receipts" => ReadReceipts?.ToString().ToLowerInvariant(),
        _ => null
    };
}

public sealed record PolicyOption(string Value, string Label);
public sealed record PolicySetting(string Field, string Feature, string Title, string? Note, IReadOnlyList<PolicyOption> Options);

public static class PrivacySettings
{
    // The same six controls, wording and order as the web inbox (site-inbox/app.js POLICY_ROWS).
    public static IReadOnlyList<PolicySetting> All { get; } =
    [
        new("unknown_calls", ServerFeatures.CallAdmission, "Calls from people you don’t know",
            "Silence keeps the call out of your way and lists it under Filtered calls.",
            [new("ring", "Ring"), new("silence", "Silence"), new("block", "Block")]),
        new("unnamed_messages", ServerFeatures.MessageFiltering, "Messages from addresses without a name",
            "Addresses like /k/… that have no handle.",
            [new("show", "Show"), new("filter", "Move to Filtered"), new("block", "Block")]),
        new("unknown_messages", ServerFeatures.MessageFiltering, "Messages from people you don’t know",
            "People you know are your contacts and anyone you have written to first.",
            [new("show", "Show"), new("filter", "Move to Filtered"), new("block", "Block")]),
        new("unnamed_calls", ServerFeatures.CallAdmission, "Calls from addresses without a name", null,
            [new("ring", "Ring"), new("silence", "Silence"), new("block", "Block")]),
        new("low_reputation", ServerFeatures.MessageFiltering, "Low-reputation senders",
            "Senders others have reported as spam.",
            [new("filter", "Filter"), new("allow", "Allow")]),
        new("call_history_days", ServerFeatures.CallHistory, "Keep call history",
            "Off stops keeping new calls. Entries older than this are deleted.",
            [new("0", "Off"), new("30", "30 days"), new("180", "180 days"), new("365", "1 year")]),
    ];

    public static IReadOnlyList<PolicySetting> For(ServerFeatures features) => All.Where(s => features.Has(s.Feature)).ToArray();

    // The JSON value a setting is written with: call history is a number of days, the rest are words.
    public static object Encode(string field, string value) =>
        field == "call_history_days" ? int.Parse(value, CultureInfo.InvariantCulture) : value;
}

public static class BlockRules
{
    public const string Confirmation = "They won't be able to message or call you, and won't be told.";

    // The rule covering a whole namespace, or null where nobody owns the namespace (a provider or
    // an open one) and a wildcard would block strangers who merely share it. Matches the web.
    public static string? WildcardFor(string? peer)
    {
        if (string.IsNullOrEmpty(peer) || peer.StartsWith("/k/", StringComparison.Ordinal) || peer.EndsWith("/*", StringComparison.Ordinal)) return null;
        var parts = peer.Split('/', StringSplitOptions.RemoveEmptyEntries);
        if (parts.Length == 0) return null;
        var parent = parts.Length == 1 ? parts[0] : string.Join('/', parts[..^1]);
        if (parent is "github" or "pp" or "k") return null;
        return "/" + parent + "/*";
    }

    public static string Describe(Contact rule) => rule.Peer.EndsWith("/*", StringComparison.Ordinal)
        ? "Everyone at " + rule.Peer[..^2] : string.IsNullOrWhiteSpace(rule.Alias) ? "Blocked" : rule.Alias!;

    // Typing guard for Settings → Blocked: an address, a handle or a /namespace/* wildcard.
    public static bool IsBlockable(string peer) => peer.EndsWith("/*", StringComparison.Ordinal)
        ? peer.Length > 3 && PostAddress.IsValid(peer[..^2])
        : PostAddress.IsValid(peer);

    public static bool IsBlocked(string peer, IReadOnlyList<Contact> contacts) =>
        ConversationBuilder.FindContact(peer, contacts)?.Admission == "block";
}

public sealed record CallLogEntry
{
    public required string Id { get; init; }
    public string? Direction { get; init; }
    public string? Outcome { get; init; }
    public string? Kind { get; init; }
    public string? Peer { get; init; }
    public string? PeerHandle { get; init; }
    public IReadOnlyList<string>? Participants { get; init; }
    public string? GroupId { get; init; }
    public long StartedAt { get; init; }
    public long? AnsweredAt { get; init; }
    public long? EndedAt { get; init; }
    public long? Duration { get; init; }
    public bool? Seen { get; init; }

    [JsonIgnore] public bool IsIncoming => Direction == "incoming";
    [JsonIgnore] public bool IsVideo => Kind == "video";
    [JsonIgnore] public string? PeerKey => !string.IsNullOrEmpty(PeerHandle) ? PeerHandle : string.IsNullOrEmpty(Peer) ? null : Peer;
    // Missed by the person looking: an incoming call nobody answered (a caller who hung up first included).
    [JsonIgnore] public bool IsMissed => IsIncoming && Outcome is "missed" or "cancelled";
    [JsonIgnore] public bool IsFiltered => Outcome == "filtered";
}

public sealed record CallLogPage(IReadOnlyList<CallLogEntry>? Entries, int? UnseenMissed, long? NextBefore);

public static class CallPresentation
{
    public static string OutcomeLabel(CallLogEntry entry) => entry.Outcome switch
    {
        "answered" => entry.IsIncoming ? "Incoming" : "Outgoing",
        "missed" => entry.IsIncoming ? "Missed" : "No answer",
        "declined" => "Declined",
        "cancelled" => entry.IsIncoming ? "Missed" : "Cancelled",
        "busy" => "Busy",
        "filtered" => "Filtered",
        "failed" => "Failed",
        _ => "In progress"
    };

    public static string KindWord(CallLogEntry entry) => entry.IsVideo ? "video" : "voice";

    public static string Duration(long seconds)
    {
        var total = Math.Max(0, seconds);
        var h = total / 3600; var m = total % 3600 / 60; var s = total % 60;
        return h > 0 ? $"{h}:{m:00}:{s:00}" : $"{m}:{s:00}";
    }

    public static string Detail(CallLogEntry entry)
    {
        var bits = new List<string> { OutcomeLabel(entry), entry.IsVideo ? "Video" : "Voice" };
        var duration = entry.Duration ?? (entry.AnsweredAt is { } a && entry.EndedAt is { } e ? e - a : null);
        if (entry.Outcome == "answered" && duration is { } d) bits.Add(Duration(d));
        var people = entry.Participants?.Count ?? 0;
        if (entry.GroupId is not null || people > 2) bits.Add(people + " people");
        return (entry.IsIncoming ? "↙ " : "↗ ") + string.Join(" · ", bits);
    }

    // The 1:1 conversation line for a missed call ("Missed video call · 14:32").
    public static string MissedLine(CallLogEntry entry) =>
        $"Missed {KindWord(entry)} call · {DateTimeOffset.FromUnixTimeSeconds(entry.StartedAt).ToLocalTime():t}";

    // Missed incoming 1:1 calls from this peer, as system lines for its default subject.
    public static IReadOnlyList<ThreadMessage> MissedCallLines(IEnumerable<CallLogEntry> entries, IReadOnlySet<string> peerKeys, string? threadId) =>
        entries.Where(e => e.IsIncoming && e.Outcome == "missed" && e.GroupId is null
                && (e.PeerHandle is { } h && peerKeys.Contains(PostAddress.Canonical(h)) || e.Peer is { } p && peerKeys.Contains(p)))
            .Select(e => new ThreadMessage("call:" + e.Id, MissedLine(e), e.StartedAt, threadId, false, System: true))
            .ToArray();
}

public sealed record AvatarUrls
{
    [JsonPropertyName("64")] public string? Small { get; init; }
    [JsonPropertyName("128")] public string? Medium { get; init; }
    [JsonPropertyName("256")] public string? Large { get; init; }
    [JsonPropertyName("512")] public string? Largest { get; init; }
    public string? Jpeg { get; init; }
}

public sealed record AvatarInfo(string Id, int Version, string Hash, AvatarUrls Urls, string? Visibility = null, long? CreatedAt = null)
{
    // WinUI decodes JPEG everywhere and WebP only with the Store extension, so lists use the JPEG.
    [JsonIgnore] public string? DisplayUrl => Urls.Jpeg ?? Urls.Large ?? Urls.Medium;
}

public static class AvatarVisibility
{
    public static IReadOnlyList<PolicyOption> Options { get; } =
        [new("everyone", "Everyone"), new("known", "People I know"), new("nobody", "Nobody")];
}

// A centred square crop, in source pixels, scaled by `zoom` (1 = the largest square that fits).
public static class AvatarCrop
{
    public static (int X, int Y, int Size) Square(int width, int height, double zoom = 1, double panX = 0.5, double panY = 0.5)
    {
        if (width <= 0 || height <= 0) throw new ArgumentOutOfRangeException(nameof(width));
        var size = Math.Max(1, (int)Math.Floor(Math.Min(width, height) / Math.Clamp(zoom, 1, 8)));
        var x = (int)Math.Round((width - size) * Math.Clamp(panX, 0, 1));
        var y = (int)Math.Round((height - size) * Math.Clamp(panY, 0, 1));
        return (x, y, size);
    }

    public static string Query(int x, int y, int size) => FormattableString.Invariant($"{x},{y},{size},{size}");
}

public static class Timers
{
    // Allowed disappearing-message timers (SDS §17), in seconds; null is Off.
    public static IReadOnlyList<int?> Choices { get; } = [null, 300, 3600, 28800, 86400, 604800, 2419200];

    public static string Label(int? seconds) => seconds switch
    {
        null or 0 => "Off",
        300 => "5 minutes",
        3600 => "1 hour",
        28800 => "8 hours",
        86400 => "1 day",
        604800 => "1 week",
        2419200 => "4 weeks",
        { } s => $"{Math.Round(s / 60d)} minutes"
    };

    public static string Banner(int? seconds) => seconds is > 0 ? $"Messages disappear after {Label(seconds)}" : "";

    public const string Limits = "Recipients can still screenshot or copy messages, notifications already shown may stay, and copies made outside Pigeonpost are not affected.";

    public static bool Expired(long? expiresAt, long now) => expiresAt is { } at && at <= now;
}

public sealed partial class PostboxClient
{
    // ---- filtering and privacy --------------------------------------------------------------

    public Task<InboxPolicy> GetPolicyAsync(string identity, CancellationToken cancellationToken) =>
        GetJsonAsync<InboxPolicy>("/v1/policy", identity, cancellationToken);

    public Task<InboxPolicy> SetPolicyAsync(string identity, string field, object value, CancellationToken cancellationToken) =>
        SendJsonAsync<InboxPolicy>(HttpMethod.Put, "/v1/policy", new Dictionary<string, object> { [field] = value, ["identity"] = identity }, identity, cancellationToken);

    public async Task<IReadOnlyList<InboxMessage>> GetFilteredAsync(string identity, CancellationToken cancellationToken) =>
        (await GetJsonAsync<InboxResponse>("/v1/inbox?filtered=1&include_read=true&actions=1&fold=1", identity, cancellationToken).ConfigureAwait(false)).Messages?
            .Where(m => !m.IsOutgoing).ToArray() ?? [];

    public Task AcceptFilteredAsync(string identity, string peer, CancellationToken cancellationToken) =>
        SendAsync(HttpMethod.Post, "/v1/filtered/accept", new { peer, identity }, identity, cancellationToken);

    public async Task<IReadOnlyList<Contact>> GetBlockedAsync(string identity, CancellationToken cancellationToken) =>
        (await GetJsonAsync<BlockedResponse>("/v1/contacts?admission=block", identity, cancellationToken).ConfigureAwait(false)).Contacts?
            .Where(c => c.Admission == "block").ToArray() ?? [];

    public Task BlockAsync(string identity, string peer, string? alias, CancellationToken cancellationToken) =>
        SendAsync(HttpMethod.Put, "/v1/contacts", new { identity, peer, alias, admission = "block", autonomy = "review", allowed_verbs = Array.Empty<string>() }, identity, cancellationToken);

    public Task UnblockAsync(string identity, string peer, CancellationToken cancellationToken) =>
        SendAsync(HttpMethod.Delete, "/v1/contacts?peer=" + Uri.EscapeDataString(peer), new { peer, identity }, identity, cancellationToken);

    public Task ReportMessageAsync(string identity, string messageId, CancellationToken cancellationToken) =>
        SendAsync(HttpMethod.Post, "/v1/report-spam", new { message_id = messageId, identity }, identity, cancellationToken);

    public Task ReportCallAsync(string identity, string callId, CancellationToken cancellationToken) =>
        SendAsync(HttpMethod.Post, "/v1/report-spam", new { call_id = callId, identity }, identity, cancellationToken);

    // ---- call history ---------------------------------------------------------------------

    public Task<CallLogPage> GetCallLogAsync(string identity, string filter, long? before, CancellationToken cancellationToken, int limit = 50) =>
        GetJsonAsync<CallLogPage>("/v1/calls/log?filter=" + Uri.EscapeDataString(filter) + "&limit=" + limit
            + (before is { } b ? "&before=" + b.ToString(CultureInfo.InvariantCulture) : ""), identity, cancellationToken);

    public async Task<int> MarkCallsSeenAsync(string identity, long upTo, CancellationToken cancellationToken) =>
        (await SendJsonAsync<SeenResponse>(HttpMethod.Post, "/v1/calls/log/seen", new { up_to = upTo, identity }, identity, cancellationToken).ConfigureAwait(false)).UnseenMissed ?? 0;

    public Task DeleteCallAsync(string identity, string id, CancellationToken cancellationToken) =>
        SendAsync(HttpMethod.Delete, "/v1/calls/log/" + Uri.EscapeDataString(id), null, identity, cancellationToken);

    public Task ClearCallsAsync(string identity, CancellationToken cancellationToken) =>
        SendAsync(HttpMethod.Delete, "/v1/calls/log", null, identity, cancellationToken);

    // ---- profile pictures -------------------------------------------------------------------

    public async Task<AvatarInfo?> GetOwnAvatarAsync(string identity, CancellationToken cancellationToken) =>
        (await GetJsonAsync<OwnAvatarResponse>("/v1/profile/avatar", identity, cancellationToken).ConfigureAwait(false)).Avatar;

    public async Task<AvatarInfo?> SetAvatarAsync(string identity, byte[] image, string mediaType, (int X, int Y, int Size)? crop, string visibility, bool all, CancellationToken cancellationToken)
    {
        if (image.Length is 0 or > 10 * 1024 * 1024) throw new PostboxException(413, "too_large");
        var query = "/v1/profile/avatar?visibility=" + Uri.EscapeDataString(visibility) + (all ? "&all=1" : "")
            + (crop is { } c ? "&crop=" + AvatarCrop.Query(c.X, c.Y, c.Size) : "");
        var body = await SendBytesAsync<OwnAvatarResponse>(HttpMethod.Put, query, image, mediaType, identity, cancellationToken).ConfigureAwait(false);
        return body.Avatar ?? body.Avatars?.FirstOrDefault(a => a.Address == identity)?.Avatar ?? body.Avatars?.FirstOrDefault()?.Avatar;
    }

    public async Task<AvatarInfo?> SetAvatarVisibilityAsync(string identity, string visibility, CancellationToken cancellationToken) =>
        (await SendJsonAsync<OwnAvatarResponse>(HttpMethod.Patch, "/v1/profile/avatar", new { visibility, identity }, identity, cancellationToken).ConfigureAwait(false)).Avatar;

    public Task RemoveAvatarAsync(string identity, CancellationToken cancellationToken) =>
        SendAsync(HttpMethod.Delete, "/v1/profile/avatar", null, identity, cancellationToken);

    // Up to 100 addresses per request; the map is keyed by the string sent, null = no picture.
    public async Task<IReadOnlyDictionary<string, AvatarInfo?>> LookupAvatarsAsync(IReadOnlyCollection<string> addresses, CancellationToken cancellationToken)
    {
        using var operation = AuthorizationContext.Capture(tokens.AcceptedAuthorization);
        var owner = new RequestAuthorization(tokens);
        var result = new Dictionary<string, AvatarInfo?>(StringComparer.Ordinal);
        foreach (var batch in addresses.Distinct(StringComparer.Ordinal).Chunk(100))
        {
            owner.Check(cancellationToken);
            var body = await ReadAsync<AvatarsResponse>("/v1/avatars?addresses=" + string.Join(',', batch.Select(Uri.EscapeDataString)), cancellationToken).ConfigureAwait(false);
            foreach (var address in batch) result[address] = body.Avatars?.GetValueOrDefault(address);
        }
        owner.Check(cancellationToken);
        return result;
    }

    public Task<byte[]> GetAvatarImageAsync(string url, CancellationToken cancellationToken)
    {
        if (!url.StartsWith("/v1/avatar-blobs/", StringComparison.Ordinal)) throw new ArgumentException("Not an avatar address.", nameof(url));
        return GetBytesAsync(url, null, cancellationToken, maxBytes: 1024 * 1024);
    }

    public Task ReportAvatarAsync(string identity, string address, CancellationToken cancellationToken) =>
        SendAsync(HttpMethod.Post, "/v1/report-avatar", new { address, identity }, identity, cancellationToken);

    // ---- disappearing messages in direct threads --------------------------------------------

    public Task SetThreadTimerAsync(string identity, string threadId, int? seconds, CancellationToken cancellationToken) =>
        SendAsync(HttpMethod.Patch, "/v1/threads/" + Uri.EscapeDataString(threadId), new TimerBody(seconds, identity), identity, cancellationToken);

    private sealed record TimerBody([property: JsonIgnore(Condition = JsonIgnoreCondition.Never)] int? EphemeralSecs, string Identity);
    private sealed record BlockedResponse(IReadOnlyList<Contact>? Contacts);
    private sealed record SeenResponse(int? UnseenMissed);
    private sealed record MailboxAvatar(string? Address, AvatarInfo? Avatar);
    private sealed record OwnAvatarResponse(AvatarInfo? Avatar, IReadOnlyList<MailboxAvatar>? Avatars);
    private sealed record AvatarsResponse(Dictionary<string, AvatarInfo?>? Avatars);
}
