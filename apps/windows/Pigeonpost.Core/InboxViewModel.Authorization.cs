namespace Pigeonpost.Core;

public sealed partial class InboxViewModel
{
    private sealed class InboxActor(AuthorizationLease? accepted)
    {
        internal readonly AuthorizationLease? Accepted = accepted;
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
            var next = new InboxActor(accepted);
            if (!ReferenceEquals(accepted, service.AcceptedAuthorization)) { next.Authorization.Retire(); return false; }
            ResetAccount(next);
            return true;
        }
        catch (AuthorizationRetiredException) { return false; }
    }

    private void ResetAccount(InboxActor next)
    {
        var prior = account;
        account = next;
        prior?.Authorization.Retire();
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
            nameof(FindSummary), nameof(CurrentMatch) }) Changed(name);
        MessagesUpdated?.Invoke(this, EventArgs.Empty);
    }

    private void TouchDraft((string Mailbox, string Peer, string Subject) key) => draftRevisions[key] = ++nextDraftRevision;
    private bool OwnsDraft((string Mailbox, string Peer, string Subject) key, long revision) => draftRevisions.GetValueOrDefault(key) == revision;
}
