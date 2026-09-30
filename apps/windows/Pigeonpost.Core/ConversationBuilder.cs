namespace Pigeonpost.Core;

public static class ConversationBuilder
{
    public static IReadOnlyList<Conversation> Build(InboxSnapshot snapshot, IReadOnlyList<PendingMessage> pending,
        IReadOnlyList<Mailbox> mailboxes, Mailbox acting)
    {
        var aliases = Aliases(snapshot.Messages);
        string Key(string peer) => aliases.GetValueOrDefault(peer, peer);
        var groups = new Dictionary<string, List<ThreadMessage>>(StringComparer.Ordinal);
        List<ThreadMessage> Group(string peer)
        {
            peer = Key(peer);
            if (!groups.TryGetValue(peer, out var group)) groups[peer] = group = [];
            return group;
        }

        var ids = new HashSet<string>(StringComparer.Ordinal);
        foreach (var m in snapshot.Messages)
        {
            if (string.IsNullOrEmpty(m.MessageId) || !ids.Add(m.MessageId)) continue;
            Group(m.PeerKey).Add(new ThreadMessage(m.MessageId, m.Body, m.At, m.ThreadId, m.IsOutgoing,
                m.IsOutgoing || m.Read == true, m.IsOutgoing ? null : m.Autonomy, m.IsOutgoing ? null : m.Verb,
                m.IsOutgoing ? null : m.HeldBecause, Attachments: m.Attachments));
        }
        foreach (var p in pending.Where(p => p.Mailbox == acting.Address))
        {
            if (p.SentCopyId is { } sentId && ids.Contains(sentId)) continue;
            if (!ids.Add(p.Id)) continue;
            Group(p.To).Add(new ThreadMessage(p.Id, p.Body, p.At, p.ThreadId, true, Status: p.Status));
        }
        foreach (var contact in snapshot.Contacts.Where(c => !c.IsWildcard)) Group(contact.Peer);

        return groups.Select(pair =>
        {
            var own = mailboxes.FirstOrDefault(m => Key(m.Key) == pair.Key || Key(m.Address) == pair.Key);
            var contact = FindContact(pair.Key, snapshot.Contacts);
            var name = own is not null ? (own.Handle is { } handle ? DisplayName(handle) : own.DisplayName)
                : string.IsNullOrWhiteSpace(contact?.Alias) ? DisplayName(pair.Key) : contact.Alias;
            var messages = pair.Value.OrderBy(m => m.At).ThenBy(m => m.Id, StringComparer.Ordinal).ToArray();
            return new Conversation(pair.Key, name, messages, own is not null, contact);
        }).OrderByDescending(c => c.Last).ThenBy(c => c.Name, StringComparer.OrdinalIgnoreCase).ToArray();
    }

    public static IReadOnlyList<Subject> Subjects(Conversation? conversation, InboxSnapshot snapshot)
    {
        if (conversation is null) return [];
        var groups = conversation.Messages.GroupBy(m => m.ThreadId ?? "", StringComparer.Ordinal)
            .ToDictionary(g => g.Key, g => new Subject(g.Key, null, g.Key.Length == 0, g.ToArray(), g.Max(m => m.At)), StringComparer.Ordinal);
        var aliases = Aliases(snapshot.Messages);
        foreach (var t in snapshot.Threads.Where(t => aliases.GetValueOrDefault(t.Peer, t.Peer) == conversation.Peer))
        {
            groups.TryGetValue(t.ThreadId, out var current);
            groups[t.ThreadId] = new Subject(t.ThreadId, t.Title, t.IsDefault == true, current?.Messages ?? [],
                Math.Max(current?.Last ?? 0, t.LastAt ?? t.CreatedAt ?? 0));
        }
        var defaultSubject = groups.Values.FirstOrDefault(s => s.IsDefault && s.Id.Length > 0);
        if (defaultSubject is not null && groups.Remove("", out var legacy))
            groups[defaultSubject.Id] = defaultSubject with
            {
                Messages = defaultSubject.Messages.Concat(legacy.Messages).OrderBy(m => m.At).ThenBy(m => m.Id, StringComparer.Ordinal).ToArray(),
                Last = Math.Max(defaultSubject.Last, legacy.Last)
            };
        return groups.Values.OrderByDescending(s => s.Last).ThenBy(s => s.Id, StringComparer.Ordinal).ToArray();
    }

    public static Contact? FindContact(string peer, IReadOnlyList<Contact> contacts)
    {
        var exact = contacts.FirstOrDefault(c => c.Peer == peer);
        if (exact is not null) return exact;
        var parts = peer.Split('/', StringSplitOptions.RemoveEmptyEntries);
        return parts.Length >= 2 ? contacts.FirstOrDefault(c => c.Peer == $"/{parts[0]}/*") : null;
    }

    private static Dictionary<string, string> Aliases(IReadOnlyList<InboxMessage> messages)
    {
        var aliases = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var m in messages)
        {
            var handle = m.PeerHandle ?? (m.IsOutgoing ? null : m.SenderHandle);
            if (string.IsNullOrEmpty(handle)) continue;
            if (m.Peer is { } peer) aliases[peer] = handle;
            var address = m.IsOutgoing ? m.To : m.From;
            if (address is not null) aliases[address] = handle;
        }
        return aliases;
    }

    private static string DisplayName(string peer) => PostAddress.DisplayName(peer);
}
