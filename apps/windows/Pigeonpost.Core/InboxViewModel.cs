namespace Pigeonpost.Core;

// Construct on the UI context. Awaited service calls return to it; stale mailbox loads are discarded.
public sealed class InboxViewModel(IInboxService service) : ObservableObject, IDisposable
{
    private InboxSnapshot snapshot = InboxSnapshot.Empty;
    private readonly List<PendingMessage> pending = [];
    private readonly Dictionary<(string Mailbox, string Peer, string Subject), string> drafts = [];
    private readonly Dictionary<string, (string? Peer, string? Subject)> selections = [];
    private CancellationTokenSource mailboxScope = new();
    private CancellationTokenSource? loading;
    private int mailboxVersion;
    private int loadVersion;
    private bool disposed;
    private bool busy;
    private bool sending;
    private bool showArchived;
    private string senderSearch = "";
    private string find = "";
    private string draft = "";
    private string? error;
    private IReadOnlyList<Conversation> allConversations = [];
    private IReadOnlyList<ThreadMessage> matches = [];
    private int matchIndex;

    public IReadOnlyList<Mailbox> Mailboxes { get; private set; } = [];
    public Mailbox? SelectedMailbox { get; private set; }
    public IReadOnlyList<Conversation> Conversations { get; private set; } = [];
    public Conversation? SelectedConversation { get; private set; }
    public IReadOnlyList<Subject> Subjects { get; private set; } = [];
    public Subject? SelectedSubject { get; private set; }
    public IReadOnlyList<ThreadMessage> Messages { get; private set; } = [];
    public bool IsBusy { get => busy; private set { if (Set(ref busy, value)) Changed(nameof(CanCompose)); } }
    public bool IsSending { get => sending; private set { if (Set(ref sending, value)) Changed(nameof(CanCompose)); } }
    public bool CanCompose => SelectedConversation is not null && !IsBusy && !IsSending;
    public bool HasConversation => SelectedConversation is not null;
    public bool HasError => !string.IsNullOrEmpty(Error);
    public string? Error { get => error; private set { if (Set(ref error, value)) Changed(nameof(HasError)); } }
    public string ConversationTitle => SelectedConversation?.Name ?? "Your agents, in one place";
    public string ConversationAddress => SelectedConversation?.Peer ?? "Select a conversation to get started.";
    public string SubjectTitle => SelectedSubject?.Name ?? "Messages";
    public string MailboxAddress => SelectedMailbox?.Key ?? "";
    public string InboxSummary => $"{allConversations.Sum(c => c.Unread)} unread · {allConversations.Sum(c => c.Held)} held";
    public string ArchiveAction => ShowArchived ? "Restore conversation" : "Archive conversation";
    public string EmptyListMessage => ShowArchived ? "No archived conversations" : SenderSearch.Length > 0 ? "No matching senders" : "No conversations yet";
    public bool IsListEmpty => Conversations.Count == 0;

    public string SenderSearch
    {
        get => senderSearch;
        set { if (Set(ref senderSearch, value)) FilterConversations(); }
    }
    public bool ShowArchived
    {
        get => showArchived;
        set { if (Set(ref showArchived, value)) { FilterConversations(); Changed(nameof(ArchiveAction)); } }
    }
    public string Draft
    {
        get => draft;
        set
        {
            if (!Set(ref draft, value)) return;
            if (DraftKey() is { } key) drafts[key] = value;
        }
    }
    public string Find
    {
        get => find;
        set { if (Set(ref find, value)) UpdateMatches(); }
    }
    public ThreadMessage? CurrentMatch => matches.Count == 0 ? null : matches[matchIndex];
    public string FindSummary => Find.Length == 0 ? "" : matches.Count == 0 ? "No matches" : $"{matchIndex + 1} of {matches.Count}";

    public async Task InitializeAsync()
    {
        IsBusy = true;
        try
        {
            Mailboxes = await service.GetMailboxesAsync(mailboxScope.Token);
            if (disposed) return;
            Changed(nameof(Mailboxes));
            if (Mailboxes.FirstOrDefault() is { } first) await SwitchMailboxAsync(first);
        }
        catch (OperationCanceledException) { }
        catch (Exception ex) { Error = Describe(ex); }
        finally { if (!disposed) IsBusy = false; }
    }

    public async Task SwitchMailboxAsync(Mailbox mailbox)
    {
        ObjectDisposedException.ThrowIf(disposed, this);
        if (SelectedMailbox?.Address == mailbox.Address) return;
        RememberSelection();
        mailboxScope.Cancel();
        mailboxScope.Dispose();
        mailboxScope = new();
        mailboxVersion++;
        SelectedMailbox = mailbox;
        snapshot = InboxSnapshot.Empty;
        allConversations = [];
        Conversations = [];
        IsSending = false;
        Error = null;
        SetSelection(null, null);
        Changed(nameof(SelectedMailbox));
        Changed(nameof(MailboxAddress));
        Changed(nameof(Conversations));
        Changed(nameof(InboxSummary));
        Changed(nameof(IsListEmpty));
        await RefreshAsync();
    }

    public async Task RefreshAsync()
    {
        if (SelectedMailbox is not { } mailbox || disposed) return;
        var generation = mailboxVersion;
        var requestVersion = ++loadVersion;
        loading?.Cancel();
        loading?.Dispose();
        loading = CancellationTokenSource.CreateLinkedTokenSource(mailboxScope.Token);
        var token = loading.Token;
        IsBusy = true;
        Error = null;
        try
        {
            var loaded = await service.LoadAsync(mailbox.Address, token);
            if (disposed || generation != mailboxVersion || requestVersion != loadVersion) return;
            snapshot = loaded;
            var serverIds = loaded.Messages.Select(m => m.MessageId).ToHashSet(StringComparer.Ordinal);
            pending.RemoveAll(p => p.Mailbox == mailbox.Address && p.SentCopyId is { } id && serverIds.Contains(id));
            Rebuild();
        }
        catch (OperationCanceledException) { }
        catch (Exception ex)
        {
            if (!disposed && generation == mailboxVersion && requestVersion == loadVersion) Error = Describe(ex);
        }
        finally
        {
            if (!disposed && generation == mailboxVersion && requestVersion == loadVersion) IsBusy = false;
        }
    }

    public void SelectConversation(Conversation? conversation)
    {
        if (SelectedConversation?.Peer == conversation?.Peer) return;
        SetSelection(conversation, null);
    }

    public void SelectSubject(Subject? subject)
    {
        if (SelectedSubject?.Id == subject?.Id) return;
        SelectedSubject = subject;
        UpdateMessages();
        RememberSelection();
    }

    public void MoveMatch(int offset)
    {
        if (matches.Count == 0) return;
        matchIndex = ((matchIndex + offset) % matches.Count + matches.Count) % matches.Count;
        Changed(nameof(CurrentMatch));
        Changed(nameof(FindSummary));
    }

    public async Task SendDraftAsync()
    {
        if (!CanCompose || SelectedMailbox is not { } mailbox || SelectedConversation is not { } conversation || string.IsNullOrWhiteSpace(Draft)) return;
        var text = Draft;
        var subject = SelectedSubject?.Id;
        var generation = mailboxVersion;
        var token = mailboxScope.Token;
        var row = new PendingMessage(Guid.NewGuid().ToString("N"), mailbox.Address, conversation.Peer,
            RequestEnvelope.Work(text.Trim()), DateTimeOffset.UtcNow.ToUnixTimeSeconds(), string.IsNullOrEmpty(subject) ? null : subject);
        pending.Add(row);
        IsSending = true;
        Error = null;
        Rebuild();
        try
        {
            var sent = await service.SendAsync(mailbox.Address, conversation.Peer, row.Body, row.ThreadId, token);
            ReplacePending(row with { Status = DeliveryStatus.Sent, SentCopyId = sent.SentCopyId });
            if (generation != mailboxVersion || disposed) return;
            if (DraftKey() == (mailbox.Address, conversation.Peer, subject ?? "") && Draft == text) Draft = "";
            await RefreshAsync();
        }
        catch (Exception ex)
        {
            ReplacePending(row with { Status = DeliveryStatus.Failed });
            if (generation != mailboxVersion || disposed) return;
            if (ex is not OperationCanceledException) Error = Describe(ex);
            Rebuild();
        }
        finally { if (!disposed && generation == mailboxVersion) IsSending = false; }
    }

    public async Task StartConversationAsync(string peer, string firstMessage)
    {
        if (SelectedMailbox is not { } mailbox || string.IsNullOrWhiteSpace(firstMessage)) return;
        peer = PostAddress.Input(peer);
        if (!PostAddress.IsValid(peer))
        {
            Error = "Use an address such as /bekir, /bekir/main or /k/your-address.";
            return;
        }
        var generation = mailboxVersion;
        var token = mailboxScope.Token;
        try
        {
            await service.SendAsync(mailbox.Address, peer, RequestEnvelope.Work(firstMessage.Trim()), null, token);
            if (disposed || generation != mailboxVersion) return;
            ShowArchived = false;
            SenderSearch = "";
            await RefreshAsync();
            if (disposed || generation != mailboxVersion) return;
            SelectConversation(Conversations.FirstOrDefault(c => c.Peer == peer));
        }
        catch (Exception ex) { if (!disposed && generation == mailboxVersion && ex is not OperationCanceledException) Error = Describe(ex); }
    }

    public async Task CreateSubjectAsync(string title)
    {
        if (SelectedMailbox is not { } mailbox || SelectedConversation is not { } conversation || string.IsNullOrWhiteSpace(title)) return;
        var generation = mailboxVersion;
        try
        {
            var id = await service.CreateThreadAsync(mailbox.Address, conversation.Peer, title.Trim(), mailboxScope.Token);
            if (disposed || generation != mailboxVersion) return;
            await RefreshAsync();
            if (disposed || generation != mailboxVersion) return;
            if (SelectedConversation?.Peer == conversation.Peer) SelectSubject(Subjects.FirstOrDefault(s => s.Id == id));
        }
        catch (Exception ex) { if (!disposed && generation == mailboxVersion && ex is not OperationCanceledException) Error = Describe(ex); }
    }

    public async Task ArchiveSelectedAsync()
    {
        if (SelectedMailbox is not { } mailbox || SelectedConversation is not { } conversation) return;
        var generation = mailboxVersion;
        try
        {
            await service.SetArchivedAsync(mailbox.Address, conversation.Peer, !ShowArchived, mailboxScope.Token);
            if (!disposed && generation == mailboxVersion) await RefreshAsync();
        }
        catch (Exception ex) { if (!disposed && generation == mailboxVersion && ex is not OperationCanceledException) Error = Describe(ex); }
    }

    public async Task AcknowledgeSelectedAsync()
    {
        if (SelectedMailbox is not { } mailbox || SelectedConversation is not { } conversation) return;
        var unread = conversation.Messages.Where(m => !m.IsOutgoing && !m.Read).ToArray();
        if (unread.Length == 0) return;
        var generation = mailboxVersion;
        var token = mailboxScope.Token;
        try
        {
            foreach (var message in unread) await service.AcknowledgeAsync(mailbox.Address, message.Id, token);
            if (!disposed && generation == mailboxVersion) await RefreshAsync();
        }
        catch (Exception ex) { if (!disposed && generation == mailboxVersion && ex is not OperationCanceledException) Error = Describe(ex); }
    }

    private void Rebuild()
    {
        if (SelectedMailbox is null) return;
        allConversations = ConversationBuilder.Build(snapshot, pending, Mailboxes, SelectedMailbox);
        FilterConversations();
        Changed(nameof(InboxSummary));
    }

    private void FilterConversations()
    {
        var remembered = SelectedMailbox is { } mailbox ? selections.GetValueOrDefault(mailbox.Address) : default;
        var peer = SelectedConversation?.Peer ?? remembered.Peer;
        var subject = SelectedSubject?.Id ?? remembered.Subject;
        Conversations = allConversations.Where(c => snapshot.Archived.Contains(c.Peer) == ShowArchived
            && (string.IsNullOrWhiteSpace(SenderSearch) || c.Name.Contains(SenderSearch.Trim(), StringComparison.OrdinalIgnoreCase)
                || c.Peer.Contains(SenderSearch.Trim(), StringComparison.OrdinalIgnoreCase))).ToArray();
        Changed(nameof(Conversations));
        Changed(nameof(IsListEmpty));
        Changed(nameof(EmptyListMessage));
        SetSelection(Conversations.FirstOrDefault(c => c.Peer == peer) ?? Conversations.FirstOrDefault(), subject);
    }

    private void SetSelection(Conversation? conversation, string? subjectId)
    {
        var peerChanged = SelectedConversation?.Peer != conversation?.Peer;
        SelectedConversation = conversation;
        Subjects = ConversationBuilder.Subjects(conversation, snapshot);
        SelectedSubject = Subjects.FirstOrDefault(s => s.Id == subjectId) ?? Subjects.FirstOrDefault();
        if (peerChanged) Find = "";
        Changed(nameof(SelectedConversation));
        Changed(nameof(Subjects));
        Changed(nameof(ConversationTitle));
        Changed(nameof(ConversationAddress));
        Changed(nameof(HasConversation));
        Changed(nameof(CanCompose));
        UpdateMessages();
        RememberSelection();
    }

    private void UpdateMessages()
    {
        var next = SelectedSubject?.Messages ?? SelectedConversation?.Messages ?? [];
        var messagesChanged = !Messages.SequenceEqual(next);
        if (messagesChanged) Messages = next;
        Changed(nameof(SelectedSubject));
        Changed(nameof(SubjectTitle));
        if (messagesChanged) Changed(nameof(Messages));
        draft = DraftKey() is { } key ? drafts.GetValueOrDefault(key, "") : "";
        Changed(nameof(Draft));
        UpdateMatches();
    }

    private void UpdateMatches()
    {
        matches = string.IsNullOrWhiteSpace(Find) ? [] : Messages.Where(m => m.DisplayBody.Contains(Find.Trim(), StringComparison.OrdinalIgnoreCase)).ToArray();
        matchIndex = 0;
        Changed(nameof(CurrentMatch));
        Changed(nameof(FindSummary));
    }

    private (string Mailbox, string Peer, string Subject)? DraftKey() => SelectedMailbox is { } mailbox && SelectedConversation is { } conversation
        ? (mailbox.Address, conversation.Peer, SelectedSubject?.Id ?? "") : null;
    private void RememberSelection()
    {
        if (SelectedMailbox is { } mailbox && SelectedConversation is { } conversation)
            selections[mailbox.Address] = (conversation.Peer, SelectedSubject?.Id);
    }
    private void ReplacePending(PendingMessage row)
    {
        var index = pending.FindIndex(p => p.Id == row.Id);
        if (index >= 0) pending[index] = row;
    }
    private static string Describe(Exception ex) => ex is PostboxException ? ex.Message : "Could not complete that action. Please try again.";

    public void Dispose()
    {
        if (disposed) return;
        disposed = true;
        mailboxScope.Cancel();
        loading?.Cancel();
        loading?.Dispose();
        mailboxScope.Dispose();
    }
}
