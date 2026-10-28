using System.Text.RegularExpressions;
using Pigeonpost.Core.Meetings;

namespace Pigeonpost.Core;

// Phase 7 meeting notes (docs/meeting-summaries.md §2, "Postbox"): account routes, read with the
// person's own session (never a mailbox token or a connector). The postbox returns a brief only
// to an attendee who agreed, as ciphertext with that account's own wraps.
public sealed partial class PostboxClient
{
    [GeneratedRegex("^mtg_[a-z0-9]{26}$")] private static partial Regex MeetingPattern();
    public static bool ValidMeetingId(string? id) => id is not null && MeetingPattern().IsMatch(id);

    // The signed-in person this login belongs to (a hash), or null when the session can't say.
    internal string? OwnerKey => tokens.RecoveryOwnerKey;

    internal async Task<IReadOnlyList<MeetingSummary>> ListMeetingsAsync(CancellationToken cancellationToken) =>
        (await GetJsonAsync<MeetingsBody>("/v1/meetings", null, cancellationToken).ConfigureAwait(false)).Meetings?.Where(m => ValidMeetingId(m.Id)).ToArray() ?? [];

    internal async Task<MeetingRecord> GetMeetingAsync(string id, CancellationToken cancellationToken)
    {
        if (!ValidMeetingId(id)) throw new ArgumentException("Not a meeting id.", nameof(id));
        var record = await GetJsonAsync<MeetingRecord>("/v1/meetings/" + id, null, cancellationToken).ConfigureAwait(false);
        return record.Id == id ? record : throw new PostboxException(502, "bad_response");
    }

    internal Task DeleteMeetingAsync(string id, CancellationToken cancellationToken) => ValidMeetingId(id)
        ? SendAsync(HttpMethod.Delete, "/v1/meetings/" + id, null, null, cancellationToken)
        : throw new ArgumentException("Not a meeting id.", nameof(id));

    internal Task RegisterNotesKeyAsync(NotesDeviceKey key, string label, CancellationToken cancellationToken) =>
        SendAsync(HttpMethod.Put, "/v1/meetings/keys/" + key.KeyId, new { public_key = key.PublicKey, label }, null, cancellationToken);

    private sealed record MeetingsBody(IReadOnlyList<MeetingSummary>? Meetings);
}
