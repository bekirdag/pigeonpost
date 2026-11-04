namespace Pigeonpost.Core.Groups;

// SDS §14C in groups (docs/message-actions.md §5). Local projections trust only references that
// were verified inside the encrypted body and belong to the same sender; the server's replacement
// hints grant no authority. Mirrors Android's GroupActions so every client folds, edits and ticks
// the same way.
public static class GroupActions
{
    public const long EditWindowSeconds = 86_400;

    // A positive decimal seq exactly as written ("012" or "+5" are not references).
    public static long? Sequence(string? value) =>
        long.TryParse(value, System.Globalization.NumberStyles.None, System.Globalization.CultureInfo.InvariantCulture, out var n)
        && n > 0 && n.ToString(System.Globalization.CultureInfo.InvariantCulture) == value ? n : null;

    private static Dictionary<long, GroupLine> Indexed(IEnumerable<GroupLine> history)
    {
        var map = new Dictionary<long, GroupLine>();
        foreach (var line in history)
            if (line.Kind == "message" && line.Seq > 0) map.TryAdd(line.Seq, line);
        return map;
    }

    // The version this one replaced: an earlier message by the same sender that is still readable.
    private static GroupLine? Parent(GroupLine line, Dictionary<long, GroupLine> bySeq)
    {
        if (line.Kind != "message" || !line.ActionsVerified || line.Pending || line.Seq <= 0 || line.Sender is not { } sender) return null;
        if (Sequence(line.EditOf) is not { } previous || previous >= line.Seq) return null;
        return bySeq.TryGetValue(previous, out var parent) && parent.Sender == sender && !parent.Deleted && !parent.Undecryptable && parent.Text is not null
            ? parent : null;
    }

    // Oldest first: the original, then every edit this device decrypted, ending with `line`.
    public static IReadOnlyList<GroupLine> Versions(GroupLine line, IReadOnlyList<GroupLine> history)
    {
        var bySeq = Indexed(history);
        var chain = new List<GroupLine> { line };
        var seen = new HashSet<long> { line.Seq };
        for (var current = Parent(line, bySeq); current is not null && seen.Add(current.Seq); current = Parent(current, bySeq))
            chain.Add(current);
        chain.Reverse();
        return chain;
    }

    // Whether `line` has been replaced by a verified later version.
    public static bool Replaced(GroupLine line, IReadOnlyList<GroupLine> history)
    {
        if (line.Kind != "message" || line.Seq <= 0) return false;
        var bySeq = Indexed(history);
        return history.Any(other => !ReferenceEquals(other, line) && Parent(other, bySeq) is { } parent && ReferenceEquals(parent, line));
    }

    // The deadline of a version is the earliest of its own and its chain's (an edit never outlives
    // the original's timer).
    public static long? Deadline(GroupLine line, IReadOnlyList<GroupLine> history)
    {
        long? min = null;
        foreach (var v in Versions(line, history))
            if (v.ExpiresAt is { } at) min = min is { } m ? Math.Min(m, at) : at;
        return min;
    }

    // §14C.3: own, sent, readable, the latest version, unexpired, and within 24 hours of the original.
    public static bool Editable(GroupLine line, IReadOnlyList<GroupLine> history, string me, long now)
    {
        if (line.Kind != "message" || line.Sender != me || line.Seq <= 0 || line.Pending || line.Deleted || line.Undecryptable || line.Text is null) return false;
        if (line.ReplacedBy is not null || Replaced(line, history)) return false;
        if (Deadline(line, history) is { } deadline && deadline <= now) return false;
        var first = Versions(line, history)[0];
        return first.EditOf is null && first.CreatedAt > 0 && now >= first.CreatedAt && now - first.CreatedAt < EditWindowSeconds;
    }

    // The message a reply quotes, when this device can show it.
    public static GroupLine? ReplyParent(GroupLine line, IReadOnlyList<GroupLine> history, long now)
    {
        if (line.Kind != "message" || Sequence(line.ReplyTo) is not { } seq || (line.Seq > 0 && seq >= line.Seq)) return null;
        var parent = Indexed(history).GetValueOrDefault(seq);
        if (parent is null || parent.Deleted || parent.Undecryptable || parent.Text is null) return null;
        if (Deadline(parent, history) is { } deadline && deadline <= now) return null;
        return parent;
    }

    // Ticks (docs/message-actions.md §5): delivered when every listed member listed it; seen (green)
    // when every member with a visible read cursor read it and at least one is visible.
    public static string Receipt(GroupLine line, GroupReceipts? receipts)
    {
        if (line.Pending || line.Seq <= 0) return "sending";
        var members = receipts?.Members ?? [];
        if (members.Count == 0 || members.Any(m => (m.DeliveredSeq ?? 0) < line.Seq)) return "sent";
        var visible = receipts?.ReadReceipts == true ? members.Where(m => m.SeenSeq is not null).Select(m => m.SeenSeq!.Value).ToArray() : [];
        return visible.Length > 0 && visible.All(s => s >= line.Seq) ? "seen" : "delivered";
    }

    public static string ReceiptLabel(string state) => state switch
    {
        "sending" => "Sending",
        "seen" => "Seen",
        "delivered" => "Delivered",
        _ => "Sent",
    };

    // What a refused group post means for the person, by what they were doing. "feature_disabled"
    // on an edit or a reply is that feature being off for the account, not groups being off.
    public static string RefusalText(PostboxException ex, GroupComposerKind? kind) => (ex.StatusCode, ex.Code) switch
    {
        (403, "feature_disabled") when kind == GroupComposerKind.Edit => "Editing messages isn't turned on for your account.",
        (403, "feature_disabled") when kind == GroupComposerKind.Reply => "Replying to messages isn't turned on for your account.",
        (404, "feature_disabled") => "Groups aren't turned on for your account.",
        (_, "feature_disabled") => "This isn't available for your account yet.",
        (_, "not_editable") => "This message can no longer be edited. Messages can be edited for 24 hours, until they disappear.",
        (_, "posting_restricted") => "Only admins can send messages in this group.",
        (_, "org_read_only") => "This organization is read-only until its payment is settled.",
        (_, "quota_exceeded") => "This group is out of storage. Delete some files or messages and try again.",
        (_, "not_member") => "You are not an active member of this group.",
        (_, "unknown_device") => "This device is no longer in the group. It rejoins automatically in a moment.",
        (429, _) or (_, "rate_limited") => ex.RetryAfter is { } wait
            ? $"You're sending too fast. Try again in {wait} second{(wait == 1 ? "" : "s")}."
            : "You're sending too fast. Please wait a moment and try again.",
        _ => ex.Message,
    };
}

public enum GroupComposerKind { Reply, Edit }
