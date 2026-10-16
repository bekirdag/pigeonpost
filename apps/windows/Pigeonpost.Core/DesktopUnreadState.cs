namespace Pigeonpost.Core;

public sealed record InboxUpdate(Mailbox Mailbox, InboxSnapshot Snapshot);
public sealed record MessageArrival(string Mailbox, string Peer, string? ThreadId, string MessageId);

// In-memory, account-scoped state. A first snapshot establishes a baseline without replaying mail.
// Counts include all observed mailboxes and are independent of search, archive and selection.
public sealed class DesktopUnreadState
{
    private readonly Dictionary<string, HashSet<string>> seen = new(StringComparer.Ordinal);
    private readonly Dictionary<string, int> counts = new(StringComparer.Ordinal);
    public int Count => counts.Values.Sum();

    public IReadOnlyList<MessageArrival> Observe(string mailbox, IReadOnlyList<InboxMessage> messages)
    {
        var unique = messages.Where(m => !string.IsNullOrEmpty(m.MessageId)).DistinctBy(m => m.MessageId).ToArray();
        var unread = unique.Where(m => !m.IsOutgoing && m.Read != true).ToArray();
        var initialized = seen.TryGetValue(mailbox, out var known);
        known ??= new(StringComparer.Ordinal);
        var arrivals = initialized ? unread.Where(m => !known.Contains(m.MessageId))
            .Select(m => new MessageArrival(mailbox, m.PeerKey, m.ThreadId, m.MessageId)).ToArray() : [];
        known.UnionWith(unique.Select(m => m.MessageId));
        seen[mailbox] = known;
        counts[mailbox] = unread.Length;
        return arrivals;
    }

    public void Clear() { seen.Clear(); counts.Clear(); }
}
