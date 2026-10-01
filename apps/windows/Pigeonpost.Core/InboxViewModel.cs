namespace Pigeonpost.Core;

// Construct on the UI context. Awaited service calls return to it; stale mailbox loads are discarded.
public sealed class InboxViewModel(IInboxService service) : ObservableObject, IDisposable
{
    private InboxSnapshot snapshot = InboxSnapshot.Empty;
    private readonly List<PendingMessage> pending = [];
    private readonly Dictionary<(string Mailbox, string Peer, string Subject), string> drafts = [];
    private readonly Dictionary<string, (string? Peer, string? Subject)> selections = [];
    private readonly Dictionary<string, HashSet<string>> opened = [];
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
    private IReadOnlyList<ThreadMessage> history = [];
    private string? historyContext;
    public int ContextVersion { get; private set; }
    public bool CanDeleteSubject => CanCompose && !string.IsNullOrEmpty(SelectedSubject?.Id);
    public IReadOnlyList<string> GrantableVerbs => snapshot.Vocabulary?.SafeGrants ?? [];
    public IReadOnlyList<string> NeverAutoVerbs => snapshot.Vocabulary?.NeverAuto ?? [];
    public Contact? ExactContact(string peer) => snapshot.Contacts.FirstOrDefault(c => c.Peer == peer);
    public Mailbox? OwnMailbox(string peer) => Mailboxes.FirstOrDefault(m => m.Address == peer || PostAddress.Canonical(m.Key) == PostAddress.Canonical(peer));

    public IReadOnlyList<Mailbox> Mailboxes { get; private set; } = [];
    public Mailbox? SelectedMailbox { get; private set; }
    public IReadOnlyList<Conversation> Conversations { get; private set; } = [];
    public Conversation? SelectedConversation { get; private set; }
    public IReadOnlyList<Subject> Subjects { get; private set; } = [];
    public Subject? SelectedSubject { get; private set; }
    public IReadOnlyList<ThreadMessage> Messages { get; private set; } = [];
    public bool IsBusy { get => busy; private set { if (Set(ref busy, value)) { Changed(nameof(CanCompose)); Changed(nameof(CanDeleteSubject)); } } }
    public bool IsSending { get => sending; private set { if (Set(ref sending, value)) { Changed(nameof(CanCompose)); Changed(nameof(CanDeleteSubject)); } } }
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
        set { if (Set(ref find, value)) UpdateMatches(reset: true); }
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
        finally { if (!disposed && loading is null) IsBusy = false; }
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

    public async Task RefreshAsync(bool background = false)
    {
        if (SelectedMailbox is not { } mailbox || disposed) return;
        if (background && (IsBusy || IsSending || loading is not null)) return;
        var generation = mailboxVersion;
        var requestVersion = ++loadVersion;
        loading?.Cancel();
        loading?.Dispose();
        loading = CancellationTokenSource.CreateLinkedTokenSource(mailboxScope.Token);
        var token = loading.Token;
        if (!background) IsBusy = true;
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
            if (!disposed && generation == mailboxVersion && requestVersion == loadVersion)
            {
                IsBusy = false;
                loading?.Dispose();
                loading = null;
            }
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
        Find = "";
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
        if (SelectedMailbox is not { } mailbox || IsBusy || IsSending) return;
        peer = PostAddress.Canonical(PostAddress.Input(peer));
        if (!PostAddress.IsValid(peer))
        {
            Error = "Use an address such as /bekir, /bekir/main or /k/your-address.";
            return;
        }
        // Opening is local. It does not create a contact or grant the sender any permissions.
        peer = OwnMailbox(peer)?.Key ?? peer;
        if (!opened.TryGetValue(mailbox.Address, out var peers)) opened[mailbox.Address] = peers = [];
        peers.Add(peer);
        ShowArchived = snapshot.Archived.Contains(peer);
        SenderSearch = "";
        Rebuild();
        SelectConversation(Conversations.FirstOrDefault(c => c.Peer == peer));
        SelectSubject(Subjects.FirstOrDefault(s => s.IsDefault));
        if (string.IsNullOrWhiteSpace(firstMessage)) return;
        // Keep an existing draft instead of overwriting it when opening the same peer again.
        if (!string.IsNullOrEmpty(Draft))
        {
            Draft += "\n" + firstMessage;
            Error = "This conversation already had an unsent draft. Review the combined message before sending.";
            return;
        }
        Draft = firstMessage;
        await SendDraftAsync();
    }

    public async Task<bool> CreateSubjectAsync(string title)
    {
        if (!CanCompose || SelectedMailbox is not { } mailbox || SelectedConversation is not { } conversation || string.IsNullOrWhiteSpace(title)) return false;
        var generation = mailboxVersion;
        try
        {
            var id = await service.CreateThreadAsync(mailbox.Address, conversation.Peer, title.Trim(), mailboxScope.Token);
            if (disposed || generation != mailboxVersion) return false;
            await RefreshAsync();
            if (disposed || generation != mailboxVersion) return false;
            if (SelectedConversation?.Peer == conversation.Peer) SelectSubject(Subjects.FirstOrDefault(s => s.Id == id));
            return true;
        }
        catch (Exception ex) { if (!disposed && generation == mailboxVersion && ex is not OperationCanceledException) Error = Describe(ex); }
        return false;
    }

    public async Task<bool> SaveContactAsync(string peer, bool known, string? alias, bool blocked, IEnumerable<string> verbs)
    {
        if (SelectedMailbox is not { } mailbox) return false;
        var generation = mailboxVersion;
        try
        {
            if (!known && !blocked) await service.RemoveContactAsync(mailbox.Address, peer, mailboxScope.Token);
            else
            {
                var grants = blocked ? [] : verbs.Intersect(GrantableVerbs, StringComparer.Ordinal).ToArray();
                await service.SetContactAsync(mailbox.Address, new(peer, alias?.Trim(), blocked ? "block" : "allow",
                    grants.Length > 0 ? "auto" : "review", grants), mailboxScope.Token);
            }
            if (!disposed && generation == mailboxVersion) await RefreshAsync();
            return true;
        }
        catch (Exception ex) { if (!disposed && generation == mailboxVersion && ex is not OperationCanceledException) Error = Describe(ex); }
        return false;
    }

    public async Task DeleteSubjectAsync()
    {
        if (!CanDeleteSubject || SelectedMailbox is not { } mailbox || SelectedConversation is not { } conversation || SelectedSubject is not { } subject) return;
        var generation = mailboxVersion;
        try
        {
            await service.DeleteThreadAsync(mailbox.Address, subject.Id, mailboxScope.Token);
            drafts.Remove((mailbox.Address, conversation.Peer, subject.Id));
            pending.RemoveAll(p => p.Mailbox == mailbox.Address && p.ThreadId == subject.Id);
            if (!disposed && generation == mailboxVersion) await RefreshAsync();
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
        allConversations = ConversationBuilder.Build(snapshot, pending, Mailboxes, SelectedMailbox, opened.GetValueOrDefault(SelectedMailbox.Address));
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
        var previousSubject = SelectedSubject?.Id;
        SelectedConversation = conversation;
        Subjects = ConversationBuilder.Subjects(conversation, snapshot);
        SelectedSubject = Subjects.FirstOrDefault(s => s.Id == subjectId) ?? Subjects.FirstOrDefault();
        if (peerChanged || previousSubject != SelectedSubject?.Id) Find = "";
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
        var context = $"{SelectedMailbox?.Address}|{SelectedConversation?.Peer}|{SelectedSubject?.Id}";
        if (historyContext != context)
        {
            historyContext = context;
            ContextVersion++;
            matches = [];
        }
        history = next;
        Changed(nameof(SelectedSubject));
        Changed(nameof(SubjectTitle));
        Changed(nameof(CanDeleteSubject));
        if (!Messages.SequenceEqual(next)) { Messages = next; Changed(nameof(Messages)); }
        draft = DraftKey() is { } key ? drafts.GetValueOrDefault(key, "") : "";
        Changed(nameof(Draft));
        UpdateMatches();
    }

    private void UpdateMatches(bool reset = false)
    {
        var previous = reset ? null : CurrentMatch?.Id;
        matches = string.IsNullOrWhiteSpace(Find) ? [] : history.Where(m => m.DisplayBody.Contains(Find.Trim(), StringComparison.OrdinalIgnoreCase)).ToArray();
        matchIndex = Math.Max(0, matches.ToList().FindIndex(m => m.Id == previous));
        if (reset || previous != CurrentMatch?.Id) Changed(nameof(CurrentMatch));
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
    private static string Describe(Exception ex) => ex is PostboxException or SignInException ? ex.Message : "Could not complete that action. Please try again.";

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
