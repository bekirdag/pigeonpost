namespace Pigeonpost.Core;

public sealed partial class InboxViewModel
{
    private sealed class InboxActor(AuthorizationLease? accepted, string? owner)
    {
        internal readonly AuthorizationLease? Accepted = accepted;
        internal readonly string? Owner = owner; // The hashed person hint when this grant was adopted.
        internal readonly AuthorizationLease Authorization = accepted?.Child() ?? new AuthorizationLease();
    }
    private InboxActor? account;
    private AuthorizationLease? ServiceAuthorization => service.AcceptedAuthorization;
    private int initializeVersion;
    private long nextDraftRevision;
    private readonly Dictionary<(string Mailbox, string Peer, string Subject), long> draftRevisions = [];

    private bool AccountCurrent(InboxActor owner)
    {
        if (disposed || !ReferenceEquals(account, owner) || !owner.Authorization.IsActive
            || !ReferenceEquals(owner.Accepted, service.AcceptedAuthorization)) return false;
        try { AuthorizationContext.CaptureValidation(() => service.AcceptedAuthorization)(); return true; }
        catch (AuthorizationRetiredException) { return false; }
    }
    private InboxActor? ActiveAccount => account is { } owner && AccountCurrent(owner) ? owner : null;

    private sealed class InboxOperation : IDisposable
    {
        private readonly InboxViewModel model;
        private readonly InboxActor owner;
        private readonly AuthorizationContext context;
        private readonly Action validate;
        internal InboxOperation(InboxViewModel model, InboxActor owner)
        {
            this.model = model; this.owner = owner;
            context = AuthorizationContext.BindActor(owner.Accepted, owner.Authorization);
            try
            {
                validate = AuthorizationContext.CaptureValidation(() => model.ServiceAuthorization);
                Check();
            }
            catch { context.Dispose(); throw; }
        }
        internal bool IsCurrent
        {
            get { try { Check(); return true; } catch (AuthorizationRetiredException) { return false; } }
        }
        internal bool IsMailbox(int generation) => IsCurrent && generation == model.mailboxVersion;
        internal AuthorizationLease? Accepted => owner.Accepted;
        internal void Check(CancellationToken token = default)
        {
            token.ThrowIfCancellationRequested();
            validate();
            if (!model.AccountCurrent(owner)) throw new AuthorizationRetiredException();
        }
        internal void RequireService(AuthorizationLease? accepted)
        {
            Check();
            if (!ReferenceEquals(owner.Accepted, accepted)) throw new AuthorizationRetiredException();
        }
        // Only private, synchronous bookkeeping belongs inside the actor's write barrier.
        internal void Local(Action write)
        {
            Check();
            owner.Authorization.WithActive(() => { write(); return 0; });
        }
        internal bool TryLocal(Action write)
        {
            try { Local(write); return true; } catch (AuthorizationRetiredException) { return false; }
        }
        public void Dispose() => context.Dispose();
    }

    private InboxOperation? BeginOperation()
    {
        if (ActiveAccount is not { } owner) return null;
        try { return new InboxOperation(this, owner); } catch (AuthorizationRetiredException) { return null; }
    }

    private bool PrepareAccount(bool allowReplacement)
    {
        if (disposed) return false;
        var accepted = service.AcceptedAuthorization;
        try
        {
            // A callback bound to an old/no-grant context may not initialize a new login.
            AuthorizationContext.CaptureValidation(() => service.AcceptedAuthorization)();
            if (account is { } current && ReferenceEquals(current.Accepted, accepted) && current.Authorization.IsActive) return true;
            if (account is not null && !allowReplacement) return false;
            var next = new InboxActor(accepted, service.RecoveryOwnerKey);
            if (!ReferenceEquals(accepted, service.AcceptedAuthorization)) { next.Authorization.Retire(); return false; }
            var carry = carried; carried = null; restoring = false; restoredMailbox = null;
            ResetAccount(next);
            if (carry is not null && carry.MayFollow(accepted, service.RecoveryOwnerKey)) RestoreUnsentWork(carry);
            return true;
        }
        catch (AuthorizationRetiredException) { return false; }
    }

    private void ResetAccount(InboxActor next)
    {
        var prior = account;
        account = next;
        prior?.Authorization.Retire();
        ClearComposerAction(restore: false);
        mailboxVersion++; loadVersion++;
        mailboxScope.Cancel(); mailboxScope.Dispose(); mailboxScope = new();
        var previousLoad = loading; loading = null;
        previousLoad?.Cancel(); previousLoad?.Dispose();
        SelectedMailbox = null;
        SelectedConversation = null;
        SelectedSubject = null;
        Mailboxes = [];
        snapshot = InboxSnapshot.Empty;
        allConversations = [];
        callEntries = [];
        pending.Clear(); drafts.Clear(); attachmentDrafts.Clear(); selections.Clear(); opened.Clear(); draftRevisions.Clear();
        deliveryDraftOwners.Clear(); sendingActivity = null; ClearPendingDeliveryViews();
        messageFeatures = ServerFeatures.None;
        history = []; historyContext = null; matches = []; matchIndex = 0;
        ContextVersion++;
        var wasUpdating = IsUpdatingLists;
        IsUpdatingLists = true;
        try
        {
            Conversations.Clear(); Subjects.Clear(); Messages.Clear(); DraftAttachments.Clear();
            Set(ref draft, "", nameof(Draft));
            Set(ref senderSearch, "", nameof(SenderSearch));
            Set(ref find, "", nameof(Find));
            Set(ref showArchived, false, nameof(ShowArchived));
            IsSending = false; IsBusy = false; Error = null;
        }
        finally { IsUpdatingLists = wasUpdating; }
        foreach (var name in new[] { nameof(Mailboxes), nameof(SelectedMailbox), nameof(SelectedConversation), nameof(SelectedSubject),
            nameof(MailboxAddress), nameof(ConversationTitle), nameof(ConversationAddress), nameof(SubjectTitle), nameof(HasConversation),
            nameof(InboxSummary), nameof(IsListEmpty), nameof(EmptyListMessage), nameof(HasDraftAttachments), nameof(CanCompose),
            nameof(CanDeleteSubject), nameof(IsSelectedBlocked), nameof(BlockedBanner), nameof(CanSetTimer), nameof(TimerBanner), nameof(HasTimer),
            nameof(FindSummary), nameof(CurrentMatch), nameof(CanInspectMessages) }) Changed(name);
        MessagesUpdated?.Invoke(this, EventArgs.Empty);
    }

    // Unsent text and file drafts survive one deliberate same-person grant replacement (a fresh
    // verification). Nothing else is carried: server data, pending views and composer actions reload
    // under the new grant, and drafts for an inbox the new grant no longer lists are dropped.
    public sealed class UnsentWork
    {
        internal readonly string OwnerKey;
        internal readonly AuthorizationLease Prior;
        internal readonly string? Mailbox;
        internal readonly Dictionary<(string Mailbox, string Peer, string Subject), string> Drafts;
        internal readonly Dictionary<(string Mailbox, string Peer, string Subject), List<DraftAttachment>> Files;
        internal readonly Dictionary<string, (string? Peer, string? Subject)> Selections;
        internal UnsentWork(string owner, AuthorizationLease prior, string? mailbox,
            Dictionary<(string, string, string), string> drafts, Dictionary<(string, string, string), List<DraftAttachment>> files,
            Dictionary<string, (string?, string?)> selections)
        { OwnerKey = owner; Prior = prior; Mailbox = mailbox; Drafts = drafts; Files = files; Selections = selections; }
        public bool IsEmpty => Drafts.Values.All(d => d.Length == 0) && Files.Values.All(f => f.Count == 0);
        // Only a different, live grant of the same person may receive these drafts.
        internal bool MayFollow(AuthorizationLease? accepted, string? owner) => accepted is { IsActive: true }
            && !ReferenceEquals(accepted, Prior) && !accepted.SharesAuthorization(Prior) && owner is not null && owner == OwnerKey;
        public override string ToString() => "UnsentWork(redacted)";
    }
    private UnsentWork? carried;
    private string? restoredMailbox;
    private bool restoring;

    // A send, an edit or a reply is bound to the current grant: finish or cancel it before a
    // deliberate grant replacement starts.
    public bool ReadyForGrantReplacement => ActiveAccount is not null && !IsSending && composerMode is null && !(IsBusy && loading is null);

    // After a verified same-person adoption retired `prior`, carry this model's unsent drafts (as
    // they are now, including typing during verification) into the next InitializeAsync.
    public void DropCarriedWork() { carried = null; restoring = false; restoredMailbox = null; }
    public bool CarryUnsentWork(AuthorizationLease prior)
    {
        if (disposed || account is not { Owner: { } person } owner || !ReferenceEquals(owner.Accepted, prior)) return false;
        RememberSelection();
        carried = new(person, prior, SelectedMailbox?.Address,
            drafts.Where(p => p.Value.Length > 0).ToDictionary(p => p.Key, p => p.Value),
            attachmentDrafts.Where(p => p.Value.Count > 0).ToDictionary(p => p.Key, p => p.Value.ToList()),
            new(selections));
        return true;
    }
    private void RestoreUnsentWork(UnsentWork work)
    {
        foreach (var (key, text) in work.Drafts) { drafts[key] = text; TouchDraft(key); }
        foreach (var (key, files) in work.Files) { attachmentDrafts[key] = files.ToList(); TouchDraft(key); }
        foreach (var (key, value) in work.Selections) selections[key] = value;
        restoredMailbox = work.Mailbox; restoring = true;
    }
    // Drafts never follow an inbox that the new grant does not list.
    private void PruneRestored(IReadOnlyList<Mailbox> listed)
    {
        var allowed = listed.Select(m => m.Address).ToHashSet(StringComparer.Ordinal);
        foreach (var key in drafts.Keys.Where(k => !allowed.Contains(k.Mailbox)).ToArray()) { drafts.Remove(key); draftRevisions.Remove(key); }
        foreach (var key in attachmentDrafts.Keys.Where(k => !allowed.Contains(k.Mailbox)).ToArray()) { attachmentDrafts.Remove(key); draftRevisions.Remove(key); }
        foreach (var key in selections.Keys.Where(k => !allowed.Contains(k)).ToArray()) selections.Remove(key);
    }

    private void TouchDraft((string Mailbox, string Peer, string Subject) key) => draftRevisions[key] = ++nextDraftRevision;
    private bool OwnsDraft((string Mailbox, string Peer, string Subject) key, long revision) => draftRevisions.GetValueOrDefault(key) == revision;
}
