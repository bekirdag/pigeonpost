namespace Pigeonpost.Core;

// Local, disposable sample data. This service has no HttpClient, credential store or network path.
public sealed class PreviewInboxService : IInboxService
{
    private readonly Dictionary<string, InboxSnapshot> snapshots;
    private int sequence;
    private const long Epoch = 1789113600;
    private readonly Mailbox[] mailboxes =
    [
        new("/k/preview-main", "/preview/main", "Main inbox"),
        new("/k/preview-team", "/preview/team", "Team inbox")
    ];

    public PreviewInboxService()
    {
        var messages = new List<InboxMessage>
        {
            Received("design-1", "/preview/design", "The desktop layout is ready: conversations on the left, subjects in the middle, and the message you are reading on the right.", "design-general", Epoch - 7200, true),
            new() { MessageId = "design-2", Direction = "out", From = mailboxes[0].Address, To = "/preview/design", Peer = "/preview/design", Body = RequestEnvelope.Work("Keep the columns resizable and make the keyboard feel at home on Windows."), ThreadId = "design-general", SentAt = Epoch - 7000 },
            Received("design-3", "/preview/design", "Agreed. The preview uses native controls, with room for long conversations and clear unread indicators.", "design-general", Epoch - 300, false),
            Received("release-1", "/preview/design", "Release checklist\n\n• Verify x64 and ARM64 builds\n• Check keyboard navigation and text scaling\n• Publish the finished MSIX through Microsoft Store", "design-release", Epoch - 900, true),
            Received("engineering-1", "/preview/engineering", "The same postbox will serve all three clients. The API contract tests are ready for the Windows port.", "engineering-general", Epoch - 600, false),
            Received("engineering-2", "/preview/engineering", "{\"v\":1,\"verb\":\"run_tests\",\"args\":{\"suite\":\"desktop\"},\"note\":\"Please check the desktop build before release.\"}", "engineering-general", Epoch - 500, false) with { Autonomy = "review", Verb = "run_tests", HeldBecause = "verb_denied" },
            Received("welcome-1", "/preview/guide", "Welcome to Pigeonpost for Windows.\n\nThis is a sample inbox. Try switching mailboxes, searching, creating a subject, or writing a message. Everything here stays in this preview and resets when you close it.", "guide-general", Epoch - 3600, true),
            Received("file-1", "/preview/guide", "Attachments will appear alongside their messages. This example shows the file metadata; downloading files is part of the live client milestone.", "guide-general", Epoch - 3500, true) with
            {
                Attachments = [new MessageAttachment("preview-attachment", "desktop-checklist.pdf", "application/pdf", 18432)]
            }
        };
        snapshots = new(StringComparer.Ordinal)
        {
            [mailboxes[0].Address] = new(messages,
                [new("design-general", "/preview/design", IsDefault: true), new("design-release", "/preview/design", "Windows release"),
                 new("engineering-general", "/preview/engineering", IsDefault: true), new("guide-general", "/preview/guide", IsDefault: true)],
                [new("/preview/design", "Design", "allow", "review"), new("/preview/engineering", "Engineering", "allow", "review"),
                 new("/preview/guide", "Getting started", "allow", "review")], new HashSet<string>(StringComparer.Ordinal)),
            [mailboxes[1].Address] = new([Received("team-1", "/preview/operations", "This mailbox has its own messages, selection and drafts. Switching back to Main inbox brings your conversation back.", "team-general", Epoch - 100, false)],
                [new("team-general", "/preview/operations", IsDefault: true)],
                [new("/preview/operations", "Operations", "allow", "review")], new HashSet<string>(StringComparer.Ordinal))
        };
    }

    public Task<IReadOnlyList<Mailbox>> GetMailboxesAsync(CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        return Task.FromResult<IReadOnlyList<Mailbox>>(mailboxes);
    }

    public Task<InboxSnapshot> LoadAsync(string identity, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        return Task.FromResult(snapshots[identity]);
    }

    public Task<SendReceipt> SendAsync(string identity, string peer, string body, string? threadId, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        var snapshot = snapshots[identity];
        EnsurePeer(ref snapshot, peer);
        threadId ??= snapshot.Threads.FirstOrDefault(t => t.Peer == peer && t.IsDefault == true)?.ThreadId;
        if (threadId is null)
        {
            threadId = "preview-thread-" + ++sequence;
            snapshot = snapshot with { Threads = [.. snapshot.Threads, new(threadId, peer, IsDefault: true)] };
        }
        var id = "preview-sent-" + ++sequence;
        snapshots[identity] = snapshot with
        {
            Messages = [.. snapshot.Messages, new InboxMessage
            {
                MessageId = id, Body = body, Direction = "out", From = identity, To = peer,
                Peer = peer, ThreadId = threadId, SentAt = Epoch + sequence
            }]
        };
        return Task.FromResult(new SendReceipt("preview-received-" + sequence, id));
    }

    public Task<string> CreateThreadAsync(string identity, string peer, string title, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        var snapshot = snapshots[identity];
        EnsurePeer(ref snapshot, peer);
        var id = "preview-thread-" + ++sequence;
        snapshots[identity] = snapshot with { Threads = [.. snapshot.Threads, new(id, peer, title, CreatedAt: Epoch + sequence)] };
        return Task.FromResult(id);
    }

    public Task SetArchivedAsync(string identity, string peer, bool archived, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        var snapshot = snapshots[identity];
        var peers = new HashSet<string>(snapshot.Archived, StringComparer.Ordinal);
        if (archived) peers.Add(peer); else peers.Remove(peer);
        snapshots[identity] = snapshot with { Archived = peers };
        return Task.CompletedTask;
    }

    public Task AcknowledgeAsync(string identity, string messageId, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        var snapshot = snapshots[identity];
        snapshots[identity] = snapshot with
        {
            Messages = snapshot.Messages.Select(m => m.MessageId == messageId ? m with { Read = true } : m).ToArray()
        };
        return Task.CompletedTask;
    }

    private static void EnsurePeer(ref InboxSnapshot snapshot, string peer)
    {
        if (!snapshot.Contacts.Any(c => c.Peer == peer))
            snapshot = snapshot with { Contacts = [.. snapshot.Contacts, new(peer, null, "allow", "review")] };
    }

    private static InboxMessage Received(string id, string peer, string body, string thread, long at, bool read) => new()
    {
        MessageId = id, Body = body, Peer = peer, PeerHandle = peer, From = peer, ThreadId = thread,
        ReceivedAt = at, Read = read, Autonomy = "review"
    };
}
