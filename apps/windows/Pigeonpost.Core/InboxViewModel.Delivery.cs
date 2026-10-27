namespace Pigeonpost.Core;

public sealed class PendingDeliveryView
{
    private readonly PreparedMessageAttempt attempt;
    private readonly Func<bool> current;
    private readonly Func<PendingDeliveryTarget?> begin;
    internal PendingDeliveryView(PreparedMessageAttempt attempt, Func<bool> current, Func<PendingDeliveryTarget?> begin)
    { this.attempt = attempt; this.current = current; this.begin = begin; }
    public string Recipient => current() ? attempt.Review.Recipient : "";
    public string CanonicalRecipient => current() ? attempt.Request.To : "";
    public string Subject => current() ? attempt.Review.SubjectLabel ?? "Messages" : "";
    public string Body => current() ? RequestEnvelope.DisplayText(attempt.Request.Body) : "";
    public string Details => current() ? $"{(attempt.Request.EditOf is not null ? "Edit" : attempt.Request.ReplyTo is not null ? "Reply" : "Message")} · {attempt.Request.Attachments?.Count ?? 0} files" : "";
    public string Label => current() ? $"{Recipient} · {Subject}" : "";
    internal bool Matches(string peer, string? thread) => current() && attempt.Review.MatchesRecipient(peer) && (attempt.Request.ThreadId ?? "") == (thread ?? "");
    public PendingDeliveryTarget? BeginAction() => current() ? begin() : null;
    public override string ToString() => "PendingDeliveryView(redacted)";
}

public sealed class PendingDeliveryTarget
{
    internal readonly InboxViewModel Owner;
    internal readonly PreparedMessageAttempt Attempt;
    private readonly Func<bool> current;
    private readonly Func<AuthorizationContext> bind;
    internal PendingDeliveryTarget(InboxViewModel owner, PreparedMessageAttempt attempt, Func<bool> current, Func<AuthorizationContext> bind)
    { Owner = owner; Attempt = attempt; this.current = current; this.bind = bind; }
    public bool IsCurrent => current();
    internal AuthorizationContext Bind()
    {
        if (!IsCurrent) throw new AuthorizationRetiredException();
        var context = bind();
        try { if (!IsCurrent) throw new AuthorizationRetiredException(); return context; }
        catch { context.Dispose(); throw; }
    }
    public override string ToString() => "PendingDeliveryTarget(redacted)";
}

public sealed partial class InboxViewModel
{
    private IReadOnlyList<PendingDeliveryView> pendingDeliveryViews = [];
    private int deliveryViewVersion;
    private int deliveryLoadVersion;
    private readonly Dictionary<string, DeliveryDraftOwner> deliveryDraftOwners = [];
    private sealed record SendingActivity(InboxActor Owner, int MailboxVersion);
    private SendingActivity? sendingActivity;
    private SendingActivity StartSendingActivity()
    {
        var activity = new SendingActivity(account ?? throw new AuthorizationRetiredException(), mailboxVersion);
        sendingActivity = activity; IsSending = true; return activity;
    }
    private void FinishSendingActivity(SendingActivity activity)
    {
        // Releasing this UI activity is distinct from admitting any private read/write or RPC.
        // Its caller may have retired; a newer account, inbox or activity can never be affected.
        if (disposed || !ReferenceEquals(sendingActivity, activity) || !ReferenceEquals(account, activity.Owner)
            || mailboxVersion != activity.MailboxVersion || !activity.Owner.Authorization.IsActive
            || activity.Owner.Accepted is { IsActive: false } || !ReferenceEquals(activity.Owner.Accepted, service.AcceptedAuthorization)) return;
        sendingActivity = null; IsSending = false;
    }
    private sealed record DeliveryDraftOwner(string DraftId, (string Mailbox, string Peer, string Subject) Key, long Revision,
        PendingMessage Row, IReadOnlyList<DraftAttachment> Files, IReadOnlyList<MessageAttachment> Uploaded, ComposerMode? Mode = null);
    public IReadOnlyList<PendingDeliveryView> PendingDeliveries => ActiveAccount is not null ? pendingDeliveryViews : [];
    public bool HasPendingDeliveries => PendingDeliveries.Count > 0;
    public string PendingDeliverySummary => $"{PendingDeliveries.Count} pending {(PendingDeliveries.Count == 1 ? "delivery" : "deliveries")} in this inbox";
    public bool DeliveryRecoveryAvailable => ActiveAccount is not null && deliveryCoordinator is not null && service.RecoveryOwnerKey is not null && SelectedMailbox is not null;
    public bool CanReviewPendingDeliveries => DeliveryRecoveryAvailable && !IsBusy && !IsSending;

    private void PendingDeliveriesChanged()
    {
        Changed(nameof(PendingDeliveries)); Changed(nameof(HasPendingDeliveries)); Changed(nameof(PendingDeliverySummary));
        Changed(nameof(CanReviewPendingDeliveries));
        Changed(nameof(DeliveryRecoveryAvailable));
    }
    private void ClearPendingDeliveryViews()
    {
        deliveryViewVersion++; deliveryLoadVersion++;
        pendingDeliveryViews = [];
        PendingDeliveriesChanged();
    }

    public async Task RefreshPendingDeliveriesAsync(bool silent = false)
    {
        if (deliveryCoordinator is null || SelectedMailbox is not { } mailbox || ActiveAccount is not { } owner) return;
        using var operation = BeginOperation();
        if (operation is null) return;
        var generation = mailboxVersion; var request = ++deliveryLoadVersion;
        try
        {
            using var scope = await MessageRecoveryScope.AuthorizeAsync(service, mailbox.Address, mailboxScope.Token);
            operation.Check();
            if (!operation.IsMailbox(generation) || request != deliveryLoadVersion) return;
            if (scope is null) { ClearPendingDeliveryViews(); return; }
            var pending = await deliveryCoordinator.GetPendingAsync(scope, mailboxScope.Token);
            operation.Check();
            if (!operation.IsMailbox(generation) || request != deliveryLoadVersion) return;
            var version = ++deliveryViewVersion;
            bool Current() => AccountCurrent(owner) && generation == mailboxVersion && version == deliveryViewVersion;
            var rows = pending.Select(attempt => new PendingDeliveryView(attempt, Current, () =>
            {
                if (!Current()) return null;
                using var admission = BeginOperation();
                if (admission is null) return null;
                return new(this, attempt, () => Current() && admission.IsCurrent, AuthorizationContext.CaptureBinding());
            })).ToArray();
            operation.Local(() => pendingDeliveryViews = rows);
            if (Current()) PendingDeliveriesChanged();
        }
        catch (OperationCanceledException) { }
        catch (Exception ex)
        {
            if (!silent && operation.IsMailbox(generation) && request == deliveryLoadVersion) Error = DescribeDeliveryFailure(ex, false);
        }
    }

    public async Task<bool> RetryPendingDeliveryAsync(PendingDeliveryTarget target)
    {
        if (!ReferenceEquals(target.Owner, this) || !target.IsCurrent || !CanReviewPendingDeliveries || SelectedMailbox is not { } mailbox) return false;
        using var binding = target.Bind();
        using var operation = BeginOperation();
        if (operation is null) return false;
        var generation = mailboxVersion;
        var activity = StartSendingActivity(); Error = null;
        try
        {
            using var scope = await MessageRecoveryScope.AuthorizeAsync(service, mailbox.Address, mailboxScope.Token) ?? throw new MessageRecoveryNotAuthorizedException();
            operation.Check(mailboxScope.Token);
            if (!target.IsCurrent) throw new AuthorizationRetiredException();
            var result = await deliveryCoordinator!.SendAsync(scope, target.Attempt, mailboxScope.Token);
            operation.Check();
            DeliveryDraftOwner? cleared = null;
            operation.Local(() =>
            {
                if (!deliveryDraftOwners.Remove(target.Attempt.Request.MsgRef, out var original) || original.DraftId != target.Attempt.Review.DraftId) return;
                ReplacePending(original.Row with { Status = DeliveryStatus.Sent, SentCopyId = result.Receipt.SentCopyId, Attachments = original.Uploaded });
                if (!OwnsDraft(original.Key, original.Revision)) return;
                if (original.Mode?.Kind == MessageComposerKind.Edit)
                { drafts[original.Key] = original.Mode.PreviousDraft; attachmentDrafts[original.Key] = original.Mode.PreviousFiles.ToList(); }
                else
                {
                    drafts[original.Key] = "";
                    if (attachmentDrafts.TryGetValue(original.Key, out var files)) files.RemoveAll(f => original.Files.Any(sent => sent.Id == f.Id));
                }
                if (original.Mode is not null && ReferenceEquals(composerMode, original.Mode)) composerMode = null;
                cleared = original;
            });
            if (cleared?.Mode is { } completedMode) completedMode.Dispose();
            if (!operation.IsMailbox(generation)) return true;
            if (cleared is { } clearedDraft && DraftKey() == clearedDraft.Key && OwnsDraft(clearedDraft.Key, clearedDraft.Revision)) Draft = drafts.GetValueOrDefault(clearedDraft.Key, "");
            NotifyComposerAction();
            await RefreshAsync();
            if (result.PendingCleanup && operation.IsMailbox(generation)) Error = "Delivered. The local pending entry could not be cleared; review pending deliveries.";
            return true;
        }
        catch (Exception ex)
        {
            if (operation.IsMailbox(generation) && ex is not OperationCanceledException) Error = DescribeDeliveryFailure(ex, true);
            return false;
        }
        finally
        {
            try { if (operation.IsMailbox(generation)) await RefreshPendingDeliveriesAsync(silent: true); }
            finally { operation.Dispose(); binding.Dispose(); FinishSendingActivity(activity); }
        }
    }

    public async Task<bool> DiscardPendingDeliveryAsync(PendingDeliveryTarget target)
    {
        if (!ReferenceEquals(target.Owner, this) || !target.IsCurrent || !CanReviewPendingDeliveries || SelectedMailbox is not { } mailbox) return false;
        using var binding = target.Bind();
        using var operation = BeginOperation();
        if (operation is null) return false;
        var generation = mailboxVersion;
        var activity = StartSendingActivity(); Error = null;
        try
        {
            using var scope = await MessageRecoveryScope.AuthorizeAsync(service, mailbox.Address, mailboxScope.Token) ?? throw new MessageRecoveryNotAuthorizedException();
            operation.Check(mailboxScope.Token);
            if (!target.IsCurrent) throw new AuthorizationRetiredException();
            await deliveryCoordinator!.DiscardAsync(scope, target.Attempt, mailboxScope.Token);
            operation.Check();
            operation.Local(() =>
            {
                if (deliveryDraftOwners.Remove(target.Attempt.Request.MsgRef, out var original)) pending.RemoveAll(p => p.Id == original.Row.Id);
            });
            if (operation.IsMailbox(generation)) Rebuild();
            return true;
        }
        catch (Exception ex)
        {
            if (operation.IsMailbox(generation) && ex is not OperationCanceledException) Error = DescribeDeliveryFailure(ex, false);
            return false;
        }
        finally
        {
            try { if (operation.IsMailbox(generation)) await RefreshPendingDeliveriesAsync(silent: true); }
            finally { operation.Dispose(); binding.Dispose(); FinishSendingActivity(activity); }
        }
    }

    private static string DescribeDeliveryFailure(Exception ex, bool sent) => ex switch
    {
        MessageAttemptConflictException or MessageAttemptUnavailableException or MessageAttemptLimitException => ex.Message,
        MessageRecoveryNotAuthorizedException => "Sign in again before recovering or preparing a protected delivery.",
        PostboxException => Describe(ex),
        _ => sent ? "Delivery could not be confirmed. Review pending deliveries before trying again."
            : "Could not prepare delivery. Your message and attachments are still in the draft."
    };
}
