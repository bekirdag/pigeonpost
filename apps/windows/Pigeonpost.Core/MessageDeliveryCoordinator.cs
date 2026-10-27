namespace Pigeonpost.Core;

public sealed record MessageDeliveryResult(SendReceipt Receipt, bool PendingCleanup)
{
    public override string ToString() => "MessageDeliveryResult(redacted)";
}

// Every delivery is a separate explicit call. Loading or preparing never replays a saved POST.
public sealed class MessageDeliveryCoordinator
{
    private readonly IProtectedMessageService service;
    private readonly IMessageAttemptStore store;
    public MessageDeliveryCoordinator(IProtectedMessageService service, IMessageAttemptStore store)
    {
        if (!store.Persistent) throw new ArgumentException("Delivery recovery requires protected persistent storage.");
        this.service = service; this.store = store;
    }

    public async Task<IReadOnlyList<PreparedMessageAttempt>> GetPendingAsync(MessageRecoveryScope scope, CancellationToken cancellationToken = default)
    {
        using var binding = scope.BindForService(service);
        var pending = await store.ListAsync(scope, cancellationToken).ConfigureAwait(false);
        scope.RequireCurrent(cancellationToken);
        return pending;
    }

    public async Task<PreparedMessageAttempt> PrepareAsync(MessageRecoveryScope scope, MessageSendRequest reviewedRequest,
        MessageDeliveryReview? review = null, CancellationToken cancellationToken = default)
    {
        using var binding = scope.BindForService(service);
        scope.RequireCurrent(cancellationToken);
        if (reviewedRequest.From != scope.Mailbox || review is not null && review.Recipient != reviewedRequest.To)
            throw new ArgumentException("Verify the original sender and reviewed recipient before delivery.");
        // Find the original reviewed handle before any new resolution. A transferred or removed
        // alias cannot conceal its old outstanding request or silently replace the intended inbox.
        var pending = await store.ListAsync(scope, cancellationToken).ConfigureAwait(false);
        scope.RequireCurrent(cancellationToken);
        if (pending.Any(p => p.Review.MatchesRecipient(reviewedRequest.To) && (p.Request.ThreadId ?? "") == (reviewedRequest.ThreadId ?? "")))
            throw new MessageAttemptConflictException();
        var route = await service.ResolveRecipientAsync(scope, reviewedRequest.To, cancellationToken).ConfigureAwait(false);
        scope.RequireCurrent(cancellationToken);
        if (route.From != scope.Mailbox || route.Address is null || !MessageRecoveryScope.CanonicalMailbox(route.Address))
            throw new PostboxException(502, "bad_response");
        var canonical = new MessageSendRequest(scope.Mailbox, route.Address, reviewedRequest.Body, reviewedRequest.ThreadId,
            reviewedRequest.Attachments, reviewedRequest.MsgRef, reviewedRequest.ReplyTo, reviewedRequest.Quote, reviewedRequest.EditOf);
        var attempt = PreparedMessageAttempt.Create(scope, canonical, review ?? new(reviewedRequest.To));
        await store.SaveAsync(scope, attempt, cancellationToken).ConfigureAwait(false);
        scope.RequireCurrent(cancellationToken);
        return attempt;
    }

    public async Task<MessageDeliveryResult> SendAsync(MessageRecoveryScope scope, PreparedMessageAttempt reviewedAttempt, CancellationToken cancellationToken = default)
    {
        using var binding = scope.BindForService(service);
        reviewedAttempt.RequireScope(scope);
        var saved = await store.LoadAsync(scope, reviewedAttempt.Request.To, reviewedAttempt.Request.ThreadId, cancellationToken).ConfigureAwait(false)
            ?? throw new MessageAttemptUnavailableException();
        scope.RequireCurrent(cancellationToken);
        if (saved.WireJson != reviewedAttempt.WireJson || saved.Review != reviewedAttempt.Review) throw new MessageAttemptConflictException();
        var receipt = await service.SendPreparedMessageAsync(scope, saved, cancellationToken).ConfigureAwait(false);
        scope.RequireCurrent();
        if (receipt.MsgRef != saved.Request.MsgRef) throw new PostboxException(502, "bad_response");
        try
        {
            await store.RemoveAsync(scope, saved.Request.To, saved.Request.ThreadId, saved.Request.MsgRef, cancellationToken).ConfigureAwait(false);
            scope.RequireCurrent();
            return new(receipt, false);
        }
        catch (Exception ex) when (ex is IOException or OperationCanceledException)
        {
            // A confirmed result stays confirmed if local cleanup fails. Never remove a newer
            // request, and never publish even a confirmed result into a retired account context.
            scope.RequireCurrent();
            return new(receipt, true);
        }
    }

    // Discard only removes this protected local intent; it cannot undo an accepted delivery.
    public async Task<bool> DiscardAsync(MessageRecoveryScope scope, PreparedMessageAttempt reviewedAttempt, CancellationToken cancellationToken = default)
    {
        using var binding = scope.BindForService(service);
        reviewedAttempt.RequireScope(scope);
        var removed = await store.RemoveAsync(scope, reviewedAttempt.Request.To, reviewedAttempt.Request.ThreadId,
            reviewedAttempt.Request.MsgRef, cancellationToken).ConfigureAwait(false);
        scope.RequireCurrent(cancellationToken);
        return removed;
    }
}
