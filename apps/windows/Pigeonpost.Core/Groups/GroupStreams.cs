using System.Text.Json.Serialization;
using System.Text.RegularExpressions;

namespace Pigeonpost.Core.Groups;

// Phase 6 live streams in groups (docs/streaming.md §2). The app only reads the postbox's stream
// records here; going live, watching and the stage run in the call host's page (live.js), which
// encrypts every frame. Who may do what is always the postbox's and the calls service's decision.

public sealed record StreamStageEntry
{
    public required string Address { get; init; }
    public string? State { get; init; } // invited | speaker
    public string? By { get; init; }
}

public sealed record GroupStream
{
    public required string Id { get; init; }
    public string? GroupId { get; init; }
    public string? Title { get; init; }
    public IReadOnlyList<string>? Sources { get; init; }
    public string? By { get; init; }
    public string? ByHandle { get; init; }
    public string? State { get; init; }
    public long? StartedAt { get; init; }
    public long? EndsBy { get; init; }
    public int? Viewers { get; init; }
    public IReadOnlyList<StreamStageEntry>? Stage { get; init; }
    public long? Revision { get; init; }
    [JsonIgnore] public bool IsLive => State == "live";
}

public static partial class GroupStreams
{
    [GeneratedRegex("^grp_[a-z2-7]{26}$")] private static partial Regex GroupPattern();
    [GeneratedRegex("^str_[a-z2-7]{26}$")] private static partial Regex StreamPattern();
    public static bool ValidGroupId(string? id) => id is not null && GroupPattern().IsMatch(id);
    public static bool ValidStreamId(string? id) => id is not null && StreamPattern().IsMatch(id);

    // The live stream of exactly this group in a server answer. A record naming another group, a
    // malformed id or an ended stream is ignored.
    public static GroupStream? LiveOf(string groupId, IReadOnlyList<GroupStream>? streams) =>
        streams?.FirstOrDefault(s => s.IsLive && ValidStreamId(s.Id) && s.GroupId == groupId && ValidGroupId(groupId));

    // This mailbox's place on the stage: "invited", "speaker", or null (a viewer).
    public static string? StageOf(GroupStream stream, string? mailbox) =>
        mailbox is null ? null : stream.Stage?.FirstOrDefault(e => e.Address == mailbox)?.State is ("invited" or "speaker") and var state ? state : null;

    // A title as plain text on one line, as the postbox bounds it (1–100 characters).
    public static string Title(string? title)
    {
        var text = new string((title ?? "").Select(c => char.IsControl(c) ? ' ' : c).ToArray()).Trim();
        return text.Length > 100 ? text[..100] : text;
    }

    // What the person typed, if the postbox would accept it as a title.
    public static string? ValidTitle(string? typed) => Title(typed) is { Length: >= 1 } title ? title : null;

    // The stream notice in a group: "Jane streamed for 34 min · 18 viewers" (groups.js).
    public static string NoticeText(string actor, IReadOnlyDictionary<string, string> metadata)
    {
        var seconds = double.TryParse(metadata.GetValueOrDefault("duration"), System.Globalization.NumberStyles.Float,
            System.Globalization.CultureInfo.InvariantCulture, out var d) && double.IsFinite(d) && d > 0 ? d : 0;
        var minutes = Math.Max(1, (long)Math.Round(Math.Min(seconds, 86_400) / 60, MidpointRounding.AwayFromZero));
        var viewers = long.TryParse(metadata.GetValueOrDefault("viewers"), out var v) && v > 0 ? Math.Min(v, 1_000_000) : 0;
        return $"{actor} streamed for {minutes} min · {viewers} {(viewers == 1 ? "viewer" : "viewers")}";
    }

    // Why the call host could not start or join a stream, in the app's words.
    public static string Explain(string? code) => code switch
    {
        "needs_e2ee" => "Live streams need a newer version of the app's web view, one that can encrypt audio and video itself.",
        "stream_restricted" or "not_host" => "You can't go live in this group. Ask an admin.",
        "stream_in_progress" => "Someone is already live in this group.",
        "stream_capacity" => "Live streams are very busy right now. Please try again in a few minutes.",
        "stream_full" => "This stream is full.",
        "stream_ended" or "no_stream" => "The stream has ended.",
        "busy" => "Leave the current call or stream first.",
        "human_required" => "Only you can do this, signed in on this device.",
        "rate_limited" => "Too many live streams started. Please wait a while and try again.",
        "group_not_ready" => "This group's encryption is still being set up. Try again in a moment.",
        "not_member" => "You are not an active member of this group.",
        "signed_out" or "identity_changed" => "Sign in again to watch or go live.",
        "NotAllowedError" => "Pigeonpost can't use your camera or microphone. Allow access in Windows Settings and try again.",
        _ => "The stream couldn't be opened. Please try again.",
    };
}
