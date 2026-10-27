namespace Pigeonpost.Core;

public enum MessageComposerKind { Reply, Edit }

public sealed partial class InboxViewModel
{
    private TimeProvider MessageClock => timeProvider ?? TimeProvider.System;
    private long MessageNow => MessageClock.GetUtcNow().ToUnixTimeSeconds();
    private ComposerMode? composerMode;
    private sealed class ComposerMode : IDisposable
    {
        internal readonly MessageComposerKind Kind;
        internal readonly MessageTarget Target;
        internal readonly InboxActor Owner;
        internal readonly (string Mailbox, string Peer, string Subject) Key;
        internal readonly int Generation, Context;
        internal readonly string PreviousDraft, Quote;
        internal readonly IReadOnlyList<DraftAttachment> PreviousFiles;
        internal readonly AuthorizationLease Authority;
        private readonly object gate = new();
        private int flights;
        private bool retiring;
        private ITimer? deadline;
        internal ComposerMode(MessageComposerKind kind, MessageTarget target, InboxActor owner,
            (string Mailbox, string Peer, string Subject) key, int generation, int context, string draft,
            IReadOnlyList<DraftAttachment> files, TimeProvider clock)
        {
            Kind = kind; Target = target; Owner = owner; Key = key; Generation = generation; Context = context;
            PreviousDraft = draft; PreviousFiles = files; Quote = RequestEnvelope.ReplyQuote(target.Message.CopyText);
            Authority = owner.Authorization.Child();
            var expires = target.Message.ExpiresAt;
            if (kind == MessageComposerKind.Edit) expires = Math.Min(expires ?? long.MaxValue, target.Message.At + 86401);
            if (expires is { } at)
            {
                var seconds = Math.Max(0, at - clock.GetUtcNow().ToUnixTimeSeconds());
                if (seconds <= TimeSpan.FromDays(49).TotalSeconds)
                    deadline = clock.CreateTimer(_ => Authority.Retire(), null, TimeSpan.FromSeconds(seconds), Timeout.InfiniteTimeSpan);
            }
        }
        internal IDisposable Bind()
        {
            lock (gate) { if (retiring) throw new AuthorizationRetiredException(); Authority.RequireActive(); flights++; }
            AuthorizationContext? original = null, local = null;
            try
            {
                original = Target.BindForAccount(); local = new AuthorizationContext(Authority);
                return new Flight(this, original, local);
            }
            catch { local?.Dispose(); original?.Dispose(); Release(); throw; }
        }
        private sealed class Flight(ComposerMode owner, AuthorizationContext original, AuthorizationContext local) : IDisposable
        {
            private bool disposed;
            public void Dispose() { if (disposed) return; disposed = true; local.Dispose(); original.Dispose(); owner.Release(); }
        }
        private void Release()
        {
            bool retire;
            lock (gate) { flights--; retire = retiring && flights == 0; }
            if (retire) { deadline?.Dispose(); Authority.Retire(); }
        }
        public void Dispose()
        {
            bool retire;
            lock (gate) { retiring = true; retire = flights == 0; }
            if (retire) { deadline?.Dispose(); Authority.Retire(); }
        }
        internal void Cancel()
        {
            lock (gate) retiring = true;
            deadline?.Dispose(); Authority.Retire();
        }
        public override string ToString() => "MessageComposerMode(redacted)";
    }

    private bool ComposerVisible(ComposerMode mode) => AccountCurrent(mode.Owner) && DraftKey() == mode.Key;
    public bool HasComposerAction => composerMode is { } mode && ComposerVisible(mode);
    public bool IsEditingMessage => HasComposerAction && composerMode!.Kind == MessageComposerKind.Edit;
    public string ComposerActionTitle => !HasComposerAction ? "" : IsEditingMessage ? "Editing message" : "Replying to message";
    public string ComposerActionQuote => HasComposerAction ? composerMode!.Quote : "";
    public string ComposerActionProblem => HasComposerAction ? ComposerProblem(composerMode!) ?? "" : "";
    public string ComposerPlaceholder => IsEditingMessage ? "Edit this message…" : "Write a message…";
    public string SendActionLabel => IsEditingMessage ? "Save edit" : "Send";
    public bool CanAttachFiles => CanCompose && !IsEditingMessage;
    public bool CanSendDraft => CanCompose && (!string.IsNullOrWhiteSpace(Draft) || HasDraftAttachments) && ComposerActionProblem.Length == 0;
    public bool CanReplyToMessage => CanCompose && deliveryCoordinator is not null && messageFeatures.Has(ServerFeatures.MessageReply);

    private string? ComposerProblem(ComposerMode mode)
    {
        if (!ComposerVisible(mode) || mode.Generation != mailboxVersion || mode.Context != ContextVersion || !mode.Target.IsAccountCurrent)
            return "Select the original message again before sending this action.";
        var original = mode.Target.Message;
        var current = Messages.FirstOrDefault(m => m.Id == original.Id && m.MsgRef == original.MsgRef && !m.IsSystem && m.Status == DeliveryStatus.Sent);
        if (current is null || Timers.Expired(current.ExpiresAt, MessageNow)) return "The original message is no longer available.";
        if (mode.Kind == MessageComposerKind.Edit)
        {
            if (!current.CanEdit(messageFeatures, MessageNow)) return "This message can no longer be edited.";
            if (current.Body != original.Body || current.EditCount != original.EditCount) return "The original message changed. Review its latest version before editing.";
        }
        else if (!messageFeatures.Has(ServerFeatures.MessageReply)) return "Replying is currently unavailable.";
        if (!mode.Authority.IsActive) return "Select the original message again before sending this action.";
        return null;
    }

    public bool BeginReply(MessageTarget target) => BeginComposerAction(target, MessageComposerKind.Reply);
    public bool BeginEdit(MessageTarget target) => BeginComposerAction(target, MessageComposerKind.Edit);
    private bool BeginComposerAction(MessageTarget target, MessageComposerKind kind)
    {
        using var operation = BeginOperation();
        if (operation is null || !CanCompose || deliveryCoordinator is null || !ReferenceEquals(target.Owner, this) || !target.IsCurrent
            || DraftKey() is not { } key || ActiveAccount is not { } owner) return false;
        if (kind == MessageComposerKind.Edit ? !target.Message.CanEdit(messageFeatures, MessageNow) : !messageFeatures.Has(ServerFeatures.MessageReply)) return false;
        if (pendingDeliveryViews.Any(p => p.Matches(key.Peer, key.Subject))) { Error = "Resolve the pending delivery in this subject before starting another action."; return false; }
        if (composerMode is not null && !CancelComposerAction()) return false;
        var mode = new ComposerMode(kind, target, owner, key, mailboxVersion, ContextVersion, Draft, DraftAttachments.ToArray(), MessageClock);
        try
        {
            operation.Local(() =>
            {
                composerMode = mode;
                if (kind == MessageComposerKind.Edit) { drafts[key] = target.Message.DisplayBody; attachmentDrafts[key] = []; TouchDraft(key); }
            });
        }
        catch { mode.Dispose(); throw; }
        if (kind == MessageComposerKind.Edit) { Set(ref draft, drafts[key], nameof(Draft)); UpdateDraftAttachments(); }
        Error = null; NotifyComposerAction(); return true;
    }

    public bool CancelComposerAction()
    {
        if (composerMode is not { } mode || !ComposerVisible(mode) || IsSending) return false;
        if (pendingDeliveryViews.Any(p => p.Matches(mode.Key.Peer, mode.Key.Subject))) { Error = "Resolve the pending delivery before cancelling this action."; return false; }
        ClearComposerAction(restore: true);
        Set(ref draft, DraftKey() is { } key ? drafts.GetValueOrDefault(key, "") : "", nameof(Draft));
        UpdateDraftAttachments(); NotifyComposerAction(); return true;
    }
    private void ClearComposerAction(bool restore)
    {
        if (composerMode is not { } mode) return;
        composerMode = null;
        if (restore && AccountCurrent(mode.Owner) && mode.Kind == MessageComposerKind.Edit)
        { drafts[mode.Key] = mode.PreviousDraft; attachmentDrafts[mode.Key] = mode.PreviousFiles.ToList(); TouchDraft(mode.Key); }
        mode.Cancel();
    }
    private void NotifyComposerAction()
    {
        foreach (var name in new[] { nameof(HasComposerAction), nameof(IsEditingMessage), nameof(ComposerActionTitle), nameof(ComposerActionQuote),
            nameof(ComposerActionProblem), nameof(ComposerPlaceholder), nameof(SendActionLabel), nameof(CanAttachFiles), nameof(CanSendDraft), nameof(CanReplyToMessage) }) Changed(name);
    }
}
