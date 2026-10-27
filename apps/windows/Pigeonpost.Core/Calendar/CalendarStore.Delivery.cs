namespace Pigeonpost.Core.Calendar;

public sealed partial class CalendarStore
{
    private object? activity;
    private object StartActivity(Operation operation)
    {
        var token = new object(); operation.Local(() => { activity = token; state = state with { Busy = true, Error = null, RevisionConflict = false }; }); Publish(); return token;
    }
    private void FinishActivity(Actor owner, object token)
    {
        if (!Current(owner) || !ReferenceEquals(activity, token)) return;
        // Release only this actor's private UI activity, even if the gesture caller retired.
        owner.Authority.WithActive(() => { if (Current(owner) && ReferenceEquals(activity, token)) { activity = null; state = state with { Busy = false }; } return true; }); Publish();
    }
    private async Task<MessageRecoveryScope> Recovery(Operation operation, CancellationToken ct)
    {
        var scope = await MessageRecoveryScope.AuthorizeAsync(inbox, operation.Owner.Mailbox, ct);
        try { operation.Check(); return scope ?? throw new MessageRecoveryNotAuthorizedException(); }
        catch { scope?.Dispose(); throw; }
    }
    public async Task<CalendarDeliveryResult?> CreateAsync(CalendarActionTarget target, CalendarAttemptKind kind,
        CalendarWritePayload payload, CancellationToken ct = default)
    {
        using var operation = Begin(target);
        if (operation is null || delivery is null || !state.CanCreate) return null;
        var token = StartActivity(operation);
        try
        {
            using var scope = await Recovery(operation, ct);
            var prepared = await delivery.PrepareAsync(scope, kind, payload, ct); operation.Check();
            var result = await delivery.SendAsync(scope, prepared, ct); operation.Check();
            await ReloadAsync(target, ct); operation.Check();
            if (result.PendingCleanup) { operation.Local(() => state = state with { Error = "Created. The saved recovery could not be cleared; review pending calendar changes." }); Publish(); }
            return result;
        }
        catch (OperationCanceledException) { return null; }
        catch (Exception error)
        {
            if (operation.IsCurrent)
            {
                operation.Local(() => state = state with { Error = error is CalendarAttemptConflictException or CalendarAttemptUnavailableException or PostboxException ? Describe(error)
                    : "Creation was not confirmed. Your form is kept. Review pending calendar changes before sending again." }); Publish();
            }
            return null;
        }
        finally
        {
            try { if (operation.IsCurrent) await RefreshPendingAsync(target); }
            finally { operation.Dispose(); FinishActivity(operation.Owner, token); }
        }
    }
    public async Task<CalendarDeliveryResult?> RetryAsync(CalendarActionTarget target, PreparedCalendarAttempt reviewed, CancellationToken ct = default)
    {
        using var operation = Begin(target);
        if (operation is null || delivery is null || state.Busy || !state.Pending.Any(saved => saved.Same(reviewed))) return null;
        var token = StartActivity(operation);
        try
        {
            using var scope = await Recovery(operation, ct);
            var result = await delivery.SendAsync(scope, reviewed, ct); operation.Check();
            await ReloadAsync(target, ct); operation.Check();
            if (result.PendingCleanup) { operation.Local(() => state = state with { Error = "Created. The saved recovery could not be cleared; review pending calendar changes." }); Publish(); }
            return result;
        }
        catch (OperationCanceledException) { return null; }
        catch (Exception error)
        { if (operation.IsCurrent) { operation.Local(() => state = state with { Error = error is HttpRequestException ? "The saved calendar change is still unconfirmed. Keep it pending or explicitly retry it later." : Describe(error) }); Publish(); } return null; }
        finally
        {
            try { if (operation.IsCurrent) await RefreshPendingAsync(target); }
            finally { operation.Dispose(); FinishActivity(operation.Owner, token); }
        }
    }
    public async Task<bool> DiscardAsync(CalendarActionTarget target, PreparedCalendarAttempt reviewed, CancellationToken ct = default)
    {
        using var operation = Begin(target);
        if (operation is null || delivery is null || state.Busy || !state.Pending.Any(saved => saved.Same(reviewed))) return false;
        var token = StartActivity(operation);
        try
        {
            using var scope = await Recovery(operation, ct);
            var removed = await delivery.DiscardAsync(scope, reviewed, ct); operation.Check(); return removed;
        }
        catch (OperationCanceledException) { return false; }
        catch (Exception error) { if (operation.IsCurrent) { operation.Local(() => state = state with { Error = Describe(error) }); Publish(); } return false; }
        finally
        {
            try { if (operation.IsCurrent) await RefreshPendingAsync(target); }
            finally { operation.Dispose(); FinishActivity(operation.Owner, token); }
        }
    }
}
