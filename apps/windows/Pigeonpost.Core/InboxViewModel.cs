using System.Collections.ObjectModel;

namespace Pigeonpost.Core;

// Construct on the UI context. Awaited service calls return to it; stale mailbox loads are discarded.
public sealed partial class InboxViewModel(IInboxService service, IAttachmentService? attachmentService = null,
    MessageDeliveryCoordinator? deliveryCoordinator = null) : ObservableObject, IDisposable
{
    private InboxSnapshot snapshot = InboxSnapshot.Empty;
    private readonly List<PendingMessage> pending = [];
    private readonly Dictionary<(string Mailbox, string Peer, string Subject), string> drafts = [];
    private readonly Dictionary<string, (string? Peer, string? Subject)> selections = [];
    private readonly Dictionary<(string Mailbox, string Peer, string Subject), List<DraftAttachment>> attachmentDrafts = [];
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
    public Contact? ExactContact(string peer) => snapshot.Contacts.FirstOrDefault(c => !c.IsWildcard
        && ConversationBuilder.ResolvePeer(c.Peer, snapshot, Mailboxes) == ConversationBuilder.ResolvePeer(peer, snapshot, Mailboxes));
    public Mailbox? OwnMailbox(string peer) => Mailboxes.FirstOrDefault(m => m.Address == peer || PostAddress.Canonical(m.Key) == PostAddress.Canonical(peer));

    public IReadOnlyList<Mailbox> Mailboxes { get; private set; } = [];
    public event EventHandler<InboxUpdate>? InboxUpdated;
    public Mailbox? SelectedMailbox { get; private set; }
    public ObservableCollection<Conversation> Conversations { get; } = [];
    // Every conversation of the mailbox, regardless of the search filter (the call client's names).
    public IReadOnlyList<Conversation> AllConversations => allConversations;
    public Conversation? SelectedConversation { get; private set; }
    public ObservableCollection<Subject> Subjects { get; } = [];
    public Subject? SelectedSubject { get; private set; }
    public ObservableCollection<ThreadMessage> Messages { get; } = [];
    public ObservableCollection<DraftAttachment> DraftAttachments { get; } = [];
    public bool HasDraftAttachments => DraftAttachments.Count > 0;
    public void AddAttachments(IEnumerable<DraftAttachment> files)
    {
        if (!CanCompose || DraftKey() is not { } key) return;
        if (!attachmentDrafts.TryGetValue(key, out var list)) attachmentDrafts[key] = list = [];
        var changed = false;
        foreach (var file in files)
            if (list.All(existing => existing.Id != file.Id)) { list.Add(file); changed = true; }
        if (changed) TouchDraft(key);
        UpdateDraftAttachments();
    }
    public void RemoveAttachment(DraftAttachment file)
    {
        if (!CanCompose || DraftKey() is not { } key) return;
        if (attachmentDrafts.TryGetValue(key, out var list) && list.RemoveAll(f => f.Id == file.Id) > 0) TouchDraft(key);
        UpdateDraftAttachments();
    }
    private void UpdateDraftAttachments()
    {
        StableList.Reconcile(DraftAttachments, DraftKey() is { } key ? attachmentDrafts.GetValueOrDefault(key) ?? [] : [],
            f => f.Id, (a, b) => ReferenceEquals(a, b));
        Changed(nameof(HasDraftAttachments));
    }
    public bool IsUpdatingLists { get; private set; }
    public event EventHandler? MessagesUpdating;
    public event EventHandler? MessagesUpdated;
    public bool IsBusy { get => busy; private set { if (Set(ref busy, value)) { Changed(nameof(CanCompose)); Changed(nameof(CanDeleteSubject)); Changed(nameof(CanReviewPendingDeliveries)); } } }
    public bool IsSending { get => sending; private set { if (Set(ref sending, value)) { Changed(nameof(CanCompose)); Changed(nameof(CanDeleteSubject)); Changed(nameof(CanReviewPendingDeliveries)); } } }
    public bool CanCompose => ActiveAccount is not null && SelectedConversation is not null && !IsBusy && !IsSending && !IsSelectedBlocked;
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
            if (disposed || ActiveAccount is null) return;
            if (!Set(ref draft, value)) return;
            if (DraftKey() is { } key) { drafts[key] = value; TouchDraft(key); }
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
        if (!PrepareAccount(allowReplacement: true)) return;
        using var operation = BeginOperation();
        if (operation is null) return;
        var intent = ++initializeVersion;
        try
        {
            IsBusy = true;
            var mailboxes = await service.GetMailboxesAsync(mailboxScope.Token);
            operation.Check();
            if (intent != initializeVersion) return;
            Mailboxes = mailboxes;
            Changed(nameof(Mailboxes));
            var target = Mailboxes.FirstOrDefault(m => m.Address == SelectedMailbox?.Address) ?? Mailboxes.FirstOrDefault();
            if (target is null) { var empty = new InboxActor(operation.Accepted); operation.Check(); ResetAccount(empty); return; }
            if (SelectedMailbox?.Address == target.Address) await RefreshAsync();
            else await SwitchMailboxAsync(target);
        }
        catch (OperationCanceledException) { }
        catch (Exception ex) { if (operation.IsCurrent && intent == initializeVersion) Error = Describe(ex); }
        finally { if (operation.IsCurrent && intent == initializeVersion && loading is null) IsBusy = false; }
    }

    public async Task SwitchMailboxAsync(Mailbox mailbox)
    {
        ObjectDisposedException.ThrowIf(disposed, this);
        if (!PrepareAccount(allowReplacement: false)) return;
        using var operation = BeginOperation();
        if (operation is null || Mailboxes.Count > 0 && Mailboxes.All(m => m.Address != mailbox.Address)) return;
        if (SelectedMailbox?.Address == mailbox.Address) return;
        RememberSelection();
        mailboxScope.Cancel();
        mailboxScope.Dispose();
        mailboxScope = new();
        mailboxVersion++;
        ClearPendingDeliveryViews();
        SelectedMailbox = mailbox;
        snapshot = InboxSnapshot.Empty;
        allConversations = [];
        callEntries = [];
        StableList.Reconcile(Conversations, [], c => c.Peer, StableList.SameConversation);
        IsSending = false;
        Error = null;
        SetSelection(null, null);
        Changed(nameof(SelectedMailbox));
        Changed(nameof(MailboxAddress));
        Changed(nameof(InboxSummary));
        Changed(nameof(IsListEmpty));
        await RefreshAsync();
        if (operation.IsCurrent && SelectedMailbox?.Address == mailbox.Address) await RefreshPendingDeliveriesAsync(silent: true);
    }

    public async Task RefreshAsync(bool background = false)
    {
        if (SelectedMailbox is not { } mailbox || disposed) return;
        using var operation = BeginOperation();
        if (operation is null) return;
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
            operation.Check();
            if (!operation.IsMailbox(generation) || requestVersion != loadVersion) return;
            snapshot = loaded;
            var serverIds = loaded.Messages.Select(m => m.MessageId).ToHashSet(StringComparer.Ordinal);
            pending.RemoveAll(p => p.Mailbox == mailbox.Address && p.SentCopyId is { } id && serverIds.Contains(id));
            Rebuild();
            InboxUpdated?.Invoke(this, new(mailbox, snapshot));
        }
        catch (OperationCanceledException) { }
        catch (Exception ex)
        {
            if (operation.IsMailbox(generation) && requestVersion == loadVersion) Error = Describe(ex);
        }
        finally
        {
            if (operation.IsMailbox(generation) && requestVersion == loadVersion)
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
        if (!CanCompose || SelectedMailbox is not { } mailbox || SelectedConversation is not { } conversation || string.IsNullOrWhiteSpace(Draft) && !HasDraftAttachments) return;
        using var operation = BeginOperation();
        if (operation is null) return;
        var text = Draft;
        var files = DraftAttachments.ToArray();
        var fileService = attachmentService ?? service as IAttachmentService;
        if (files.Length > 0 && fileService is null) { Error = "File sending is unavailable. Please try again."; return; }
        var subject = SelectedSubject?.Id;
        var subjectLabel = SelectedSubject?.Name;
        var sentDraftKey = (mailbox.Address, conversation.Peer, subject ?? "");
        var revision = draftRevisions.GetValueOrDefault(sentDraftKey);
        var generation = mailboxVersion;
        var token = mailboxScope.Token;
        var row = new PendingMessage(Guid.NewGuid().ToString("N"), mailbox.Address, conversation.Peer,
            files.Length > 0 ? RequestEnvelope.Attachment(text) : RequestEnvelope.Work(text.Trim()),
            DateTimeOffset.UtcNow.ToUnixTimeSeconds(), string.IsNullOrEmpty(subject) ? null : subject,
            Attachments: files.Select(f => new MessageAttachment(f.Id, f.Filename, "application/octet-stream", f.Bytes)).ToArray());
        pending.Add(row);
        var activity = StartSendingActivity();
        Error = null;
        Rebuild();
        var sendingMessage = false;
        PreparedMessageAttempt? prepared = null;
        var cleanupPending = false;
        try
        {
            using var recovery = deliveryCoordinator is null ? null
                : await MessageRecoveryScope.AuthorizeAsync(service, mailbox.Address, token) ?? throw new MessageRecoveryNotAuthorizedException();
            if (recovery is not null)
            {
                var outstanding = await deliveryCoordinator!.GetPendingAsync(recovery, token);
                operation.Check(token);
                if (outstanding.Any(p => p.Review.MatchesRecipient(conversation.Peer) && (p.Request.ThreadId ?? "") == (row.ThreadId ?? "")))
                    throw new MessageAttemptConflictException();
            }
            var uploaded = new List<MessageAttachment>();
            foreach (var file in files)
            {
                operation.Check(token);
                operation.RequireService(fileService!.AcceptedAuthorization);
                var bytes = await file.ReadAsync(token);
                operation.Check(token);
                var attachment = await fileService.UploadAsync(mailbox.Address, file.Filename, bytes, token);
                operation.Check(token);
                uploaded.Add(attachment);
            }
            operation.Check(token);
            SendReceipt sent;
            if (recovery is not null)
            {
                var draftId = Guid.NewGuid().ToString("N");
                prepared = await deliveryCoordinator!.PrepareAsync(recovery,
                    new(mailbox.Address, conversation.Peer, row.Body, row.ThreadId, uploaded.Select(f => f.Id)),
                    new(conversation.Peer, subjectLabel, draftId), token);
                operation.Check(token);
                operation.Local(() => deliveryDraftOwners[prepared.Request.MsgRef] = new(draftId, sentDraftKey, revision, row, files, uploaded));
                sendingMessage = true;
                var result = await deliveryCoordinator.SendAsync(recovery, prepared, token);
                sent = result.Receipt; cleanupPending = result.PendingCleanup;
            }
            else
            {
                sendingMessage = true;
                sent = files.Length == 0
                    ? await service.SendAsync(mailbox.Address, conversation.Peer, row.Body, row.ThreadId, token)
                    : await fileService!.SendAttachmentsAsync(mailbox.Address, conversation.Peer, row.Body, row.ThreadId, uploaded.Select(f => f.Id).ToArray(), token);
            }
            operation.Check(); // a completed receipt may update its original inbox after a mailbox switch
            operation.Local(() =>
            {
                ReplacePending(row with { Status = DeliveryStatus.Sent, SentCopyId = sent.SentCopyId, Attachments = uploaded });
                if (prepared is not null) deliveryDraftOwners.Remove(prepared.Request.MsgRef);
                if (!OwnsDraft(sentDraftKey, revision)) return;
                drafts[sentDraftKey] = "";
                if (attachmentDrafts.TryGetValue(sentDraftKey, out var sentFiles)) sentFiles.RemoveAll(f => files.Any(sentFile => sentFile.Id == f.Id));
            });
            if (!operation.IsMailbox(generation)) return;
            if (DraftKey() == sentDraftKey && OwnsDraft(sentDraftKey, revision)) Draft = "";
            await RefreshAsync();
            if (cleanupPending && operation.IsMailbox(generation)) Error = "Delivered. The local pending entry could not be cleared; review pending deliveries.";
        }
        catch (Exception ex)
        {
            if (!operation.IsCurrent) return;
            if (!operation.TryLocal(() => ReplacePending(row with { Status = DeliveryStatus.Failed }))) return;
            if (!operation.IsMailbox(generation)) return;
            if (ex is not OperationCanceledException) Error = deliveryCoordinator is not null ? DescribeDeliveryFailure(ex, sendingMessage)
                : ex is PostboxException ? Describe(ex)
                : sendingMessage ? "Delivery could not be confirmed. Check this conversation before trying again."
                : "Could not upload the files. Your message and attachments are still in the draft.";
            Rebuild();
        }
        finally
        {
            try { if (operation.IsMailbox(generation) && deliveryCoordinator is not null) await RefreshPendingDeliveriesAsync(silent: true); }
            finally { operation.Dispose(); FinishSendingActivity(activity); }
        }
    }

    public async Task StartConversationAsync(string peer, string firstMessage)
    {
        if (SelectedMailbox is not { } mailbox || ActiveAccount is null || IsBusy || IsSending) return;
        peer = PostAddress.Canonical(PostAddress.Input(peer));
        if (!PostAddress.IsValid(peer))
        {
            Error = "Use an address such as /bekir, /bekir/main or /k/your-address.";
            return;
        }
        // Opening is local. It does not create a contact or grant the sender any permissions.
        peer = ConversationBuilder.ResolvePeer(peer, snapshot, Mailboxes);
        if (!opened.TryGetValue(mailbox.Address, out var peers)) opened[mailbox.Address] = peers = [];
        peers.Add(peer);
        ShowArchived = snapshot.Archived.Any(p => ConversationBuilder.ResolvePeer(p, snapshot, Mailboxes) == peer);
        SenderSearch = "";
        Rebuild();
        SelectConversation(Conversations.FirstOrDefault(c => c.Peer == peer));
        SelectSubject(Subjects.FirstOrDefault(s => s.IsDefault));
        if (string.IsNullOrWhiteSpace(firstMessage)) return;
        // Keep an existing draft instead of overwriting it when opening the same peer again.
        if (!string.IsNullOrEmpty(Draft) || HasDraftAttachments)
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
        using var operation = BeginOperation();
        if (operation is null) return false;
        var generation = mailboxVersion;
        var context = ContextVersion;
        try
        {
            var id = await service.CreateThreadAsync(mailbox.Address, conversation.Peer, title.Trim(), mailboxScope.Token);
            operation.Check();
            if (!operation.IsMailbox(generation)) return false;
            await RefreshAsync();
            if (!operation.IsMailbox(generation)) return false;
            if (context == ContextVersion && SelectedConversation?.Peer == conversation.Peer) SelectSubject(Subjects.FirstOrDefault(s => s.Id == id));
            return true;
        }
        catch (Exception ex) { if (operation.IsMailbox(generation) && ex is not OperationCanceledException) Error = Describe(ex); }
        return false;
    }

    public async Task<bool> SaveContactAsync(string peer, bool known, string? alias, bool blocked, IEnumerable<string> verbs)
    {
        if (SelectedMailbox is not { } mailbox) return false;
        using var operation = BeginOperation();
        if (operation is null) return false;
        var generation = mailboxVersion;
        peer = ExactContact(peer)?.Peer ?? peer;
        try
        {
            if (!known && !blocked) await service.RemoveContactAsync(mailbox.Address, peer, mailboxScope.Token);
            else
            {
                var grants = blocked ? [] : verbs.Intersect(GrantableVerbs, StringComparer.Ordinal).ToArray();
                await service.SetContactAsync(mailbox.Address, new(peer, alias?.Trim(), blocked ? "block" : "allow",
                    grants.Length > 0 ? "auto" : "review", grants), mailboxScope.Token);
            }
            operation.Check();
            if (operation.IsMailbox(generation)) await RefreshAsync();
            return operation.IsCurrent;
        }
        catch (Exception ex) { if (operation.IsMailbox(generation) && ex is not OperationCanceledException) Error = Describe(ex); }
        return false;
    }

    public async Task DeleteSubjectAsync()
    {
        if (!CanDeleteSubject || SelectedMailbox is not { } mailbox || SelectedConversation is not { } conversation || SelectedSubject is not { } subject) return;
        using var operation = BeginOperation();
        if (operation is null) return;
        var generation = mailboxVersion;
        var messageIds = subject.Messages.Select(m => m.Id).ToHashSet(StringComparer.Ordinal);
        IsBusy = true;
        try
        {
            await service.DeleteThreadAsync(mailbox.Address, subject.Id, mailboxScope.Token);
            operation.Check();
            operation.Local(() =>
            {
                drafts.Remove((mailbox.Address, conversation.Peer, subject.Id));
                attachmentDrafts.Remove((mailbox.Address, conversation.Peer, subject.Id));
                draftRevisions.Remove((mailbox.Address, conversation.Peer, subject.Id));
                pending.RemoveAll(p => p.Mailbox == mailbox.Address && (p.ThreadId == subject.Id || messageIds.Contains(p.Id)));
            });
            if (operation.IsMailbox(generation)) await RefreshAsync();
        }
        catch (Exception ex) { if (operation.IsMailbox(generation) && ex is not OperationCanceledException) Error = Describe(ex); }
        finally { if (operation.IsMailbox(generation) && loading is null) IsBusy = false; }
    }

    public async Task ArchiveSelectedAsync()
    {
        if (SelectedMailbox is not { } mailbox || SelectedConversation is not { } conversation) return;
        using var operation = BeginOperation();
        if (operation is null) return;
        var generation = mailboxVersion;
        try
        {
            await service.SetArchivedAsync(mailbox.Address, conversation.Peer, !ShowArchived, mailboxScope.Token);
            operation.Check();
            if (operation.IsMailbox(generation)) await RefreshAsync();
        }
        catch (Exception ex) { if (operation.IsMailbox(generation) && ex is not OperationCanceledException) Error = Describe(ex); }
    }

    // A toast's "Mark as read": every unread message from that peer in that mailbox, without
    // opening the window or changing the selection. Only the account's own mailboxes are accepted.
    public async Task<int> MarkConversationReadAsync(string mailboxAddress, string peer, CancellationToken cancellationToken = default)
    {
        using var operation = BeginOperation();
        if (operation is null || Mailboxes.All(m => m.Address != mailboxAddress)) return 0;
        var own = Mailboxes;
        try
        {
            var loaded = await service.LoadAsync(mailboxAddress, cancellationToken);
            operation.Check(cancellationToken);
            var target = ConversationBuilder.ResolvePeer(peer, loaded, own);
            var unread = loaded.Messages.Where(m => !m.IsOutgoing && m.Read != true
                && ConversationBuilder.ResolvePeer(m.PeerKey, loaded, own) == target).ToArray();
            foreach (var message in unread)
            {
                operation.Check(cancellationToken);
                await service.AcknowledgeAsync(mailboxAddress, message.MessageId, cancellationToken);
                operation.Check(cancellationToken);
            }
            if (operation.IsCurrent && SelectedMailbox?.Address == mailboxAddress) await RefreshAsync(background: true);
            return operation.IsCurrent ? unread.Length : 0;
        }
        catch (OperationCanceledException) { return 0; }
    }

    // A toast's inline reply: sent as the composer would send it, into the same subject, and the
    // conversation counts as read once answered.
    public async Task<bool> ReplyFromAlertAsync(string mailboxAddress, string peer, string? threadId, string text, CancellationToken cancellationToken = default)
    {
        using var operation = BeginOperation();
        if (operation is null || string.IsNullOrWhiteSpace(text) || Mailboxes.All(m => m.Address != mailboxAddress)) return false;
        try
        {
            if (deliveryCoordinator is not null)
            {
                using var scope = await MessageRecoveryScope.AuthorizeAsync(service, mailboxAddress, cancellationToken) ?? throw new MessageRecoveryNotAuthorizedException();
                var prepared = await deliveryCoordinator.PrepareAsync(scope,
                    new(mailboxAddress, peer, RequestEnvelope.Work(text.Trim()), string.IsNullOrEmpty(threadId) ? null : threadId),
                    new(peer, "Notification reply"), cancellationToken);
                await deliveryCoordinator.SendAsync(scope, prepared, cancellationToken);
            }
            else await service.SendAsync(mailboxAddress, peer, RequestEnvelope.Work(text.Trim()), string.IsNullOrEmpty(threadId) ? null : threadId, cancellationToken);
            operation.Check(cancellationToken);
            await MarkConversationReadAsync(mailboxAddress, peer, cancellationToken);
            return operation.IsCurrent;
        }
        catch (OperationCanceledException) { return false; }
        catch (Exception ex) when (deliveryCoordinator is not null)
        {
            if (operation.IsCurrent && SelectedMailbox?.Address == mailboxAddress) Error = DescribeDeliveryFailure(ex, true);
            return false;
        }
        finally
        {
            if (deliveryCoordinator is not null && operation.IsCurrent && SelectedMailbox?.Address == mailboxAddress)
                await RefreshPendingDeliveriesAsync(silent: true);
        }
    }

    public async Task AcknowledgeSelectedAsync(Func<bool>? isVisible = null)
    {
        if (SelectedMailbox is not { } mailbox || SelectedSubject is not { } subject || isVisible?.Invoke() == false) return;
        using var operation = BeginOperation();
        if (operation is null) return;
        var context = ContextVersion;
        var unread = subject.Messages.Where(m => !m.IsOutgoing && !m.Read).ToArray();
        if (unread.Length == 0) return;
        var generation = mailboxVersion;
        var token = mailboxScope.Token;
        try
        {
            foreach (var message in unread)
            {
                if (!operation.IsMailbox(generation) || context != ContextVersion || isVisible?.Invoke() == false) break;
                await service.AcknowledgeAsync(mailbox.Address, message.Id, token);
                operation.Check(token);
            }
            if (operation.IsMailbox(generation)) await RefreshAsync();
        }
        catch (Exception ex) { if (operation.IsMailbox(generation) && ex is not OperationCanceledException) Error = Describe(ex); }
    }

    private void Rebuild()
    {
        if (SelectedMailbox is null) return;
        allConversations = ConversationBuilder.Build(snapshot, pending, Mailboxes, SelectedMailbox, opened.GetValueOrDefault(SelectedMailbox.Address), callEntries);
        FilterConversations();
        Changed(nameof(InboxSummary));
        // A block or a timer can change without the selected row changing.
        Changed(nameof(IsSelectedBlocked));
        Changed(nameof(BlockedBanner));
        Changed(nameof(CanCompose));
        Changed(nameof(CanSetTimer));
        Changed(nameof(TimerBanner));
        Changed(nameof(HasTimer));
    }

    private void FilterConversations()
    {
        var remembered = SelectedMailbox is { } mailbox ? selections.GetValueOrDefault(mailbox.Address) : default;
        var peer = SelectedConversation?.Peer ?? remembered.Peer;
        var subject = SelectedSubject?.Id ?? remembered.Subject;
        var archived = snapshot.Archived.Select(p => ConversationBuilder.ResolvePeer(p, snapshot, Mailboxes)).ToHashSet(StringComparer.Ordinal);
        var next = allConversations.Where(c => archived.Contains(c.Peer) == ShowArchived
            && (string.IsNullOrWhiteSpace(SenderSearch) || c.Name.Contains(SenderSearch.Trim(), StringComparison.OrdinalIgnoreCase)
                || c.Peer.Contains(SenderSearch.Trim(), StringComparison.OrdinalIgnoreCase))).ToArray();
        IsUpdatingLists = true;
        try
        {
            StableList.Reconcile(Conversations, next, c => c.Peer, StableList.SameConversation);
            SetSelection(Conversations.FirstOrDefault(c => c.Peer == peer) ?? Conversations.FirstOrDefault(), subject);
        }
        finally { IsUpdatingLists = false; }
        Changed(nameof(IsListEmpty));
        Changed(nameof(EmptyListMessage));
    }

    private void SetSelection(Conversation? conversation, string? subjectId)
    {
        var wasUpdating = IsUpdatingLists;
        IsUpdatingLists = true;
        try
        {
            var peerChanged = SelectedConversation?.Peer != conversation?.Peer;
            var previousSubject = SelectedSubject?.Id;
            var previousConversation = SelectedConversation;
            SelectedConversation = conversation;
            StableList.Reconcile(Subjects, ConversationBuilder.Subjects(conversation, snapshot, Mailboxes), s => s.Id, StableList.SameSubject);
            SelectedSubject = Subjects.FirstOrDefault(s => s.Id == subjectId) ?? Subjects.FirstOrDefault();
            if (peerChanged || previousSubject != SelectedSubject?.Id) Find = "";
            if (!ReferenceEquals(previousConversation, conversation))
            {
                Changed(nameof(SelectedConversation));
                Changed(nameof(IsSelectedBlocked));
                Changed(nameof(BlockedBanner));
                Changed(nameof(ConversationTitle));
                Changed(nameof(ConversationAddress));
                Changed(nameof(HasConversation));
                Changed(nameof(CanCompose));
            }
            UpdateMessages();
            RememberSelection();
        }
        finally { IsUpdatingLists = wasUpdating; }
    }

    private void UpdateMessages()
    {
        var next = SelectedSubject?.Messages ?? SelectedConversation?.Messages ?? [];
        var context = $"{SelectedMailbox?.Address}|{SelectedConversation?.Peer}|{SelectedSubject?.Id}";
        var contextChanged = historyContext != context;
        if (contextChanged)
        {
            historyContext = context;
            ContextVersion++;
            matches = [];
        }
        Changed(nameof(SelectedSubject));
        Changed(nameof(SubjectTitle));
        Changed(nameof(CanDeleteSubject));
        Changed(nameof(TimerBanner));
        Changed(nameof(HasTimer));
        if (contextChanged || !StableList.SameMessages(Messages, next))
        {
            MessagesUpdating?.Invoke(this, EventArgs.Empty);
            StableList.Reconcile(Messages, next, m => m.Id, StableList.SameMessage);
            MessagesUpdated?.Invoke(this, EventArgs.Empty);
        }
        history = Messages;
        Set(ref draft, DraftKey() is { } key ? drafts.GetValueOrDefault(key, "") : "", nameof(Draft));
        UpdateDraftAttachments();
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
        account?.Authorization.Retire();
        account = null;
        mailboxScope.Cancel();
        loading?.Cancel();
        loading?.Dispose();
        mailboxScope.Dispose();
    }
}
