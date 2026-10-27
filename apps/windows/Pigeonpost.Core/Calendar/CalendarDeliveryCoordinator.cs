namespace Pigeonpost.Core.Calendar;

public sealed record CalendarCreateReceipt(CalendarAttemptKind Kind, CalendarFolder? Calendar = null, CalendarEventResponse? Event = null)
{ public override string ToString() => "CalendarCreateReceipt(redacted)"; }
public sealed record CalendarDeliveryResult(CalendarCreateReceipt Receipt, bool PendingCleanup);

public interface IProtectedCalendarService : IInboxService
{
    Task<ServerFeatures> GetCapabilitiesAsync(CancellationToken cancellationToken);
    Task<CalendarCreateReceipt> SendPreparedCalendarAsync(MessageRecoveryScope scope, PreparedCalendarAttempt attempt, CancellationToken cancellationToken);
}

public sealed class CalendarDeliveryCoordinator(IProtectedCalendarService service, ICalendarAttemptStore store, TimeProvider? timeProvider = null)
{
    private long Now => (timeProvider ?? TimeProvider.System).GetUtcNow().ToUnixTimeSeconds();
    public async Task<PreparedCalendarAttempt> PrepareAsync(MessageRecoveryScope scope, CalendarAttemptKind kind,
        CalendarWritePayload payload, CancellationToken cancellationToken = default)
    {
        using var binding = scope.BindForService(service);
        scope.RequireCurrent(cancellationToken);
        if (!store.Persistent) throw new CalendarAttemptUnavailableException();
        _ = payload.RequestKey();
        if (!Enum.IsDefined(kind)) throw new ArgumentException("Choose a valid calendar change.");
        var features = await service.GetCapabilitiesAsync(cancellationToken).ConfigureAwait(false);
        scope.RequireCurrent(cancellationToken);
        if (!features.Has(ServerFeatures.Calendar)) throw new PostboxException(403, "feature_disabled");
        var prepared = new PreparedCalendarAttempt(scope, kind, payload, Now);
        await store.SaveAsync(scope, prepared, cancellationToken).ConfigureAwait(false);
        scope.RequireCurrent(cancellationToken); return prepared;
    }
    public async Task<IReadOnlyList<PreparedCalendarAttempt>> GetPendingAsync(MessageRecoveryScope scope, CancellationToken cancellationToken = default)
    {
        using var binding = scope.BindForService(service);
        var result = await store.ListAsync(scope, cancellationToken).ConfigureAwait(false);
        scope.RequireCurrent(cancellationToken); return result;
    }
    public async Task<CalendarDeliveryResult> SendAsync(MessageRecoveryScope scope, PreparedCalendarAttempt attempt, CancellationToken cancellationToken = default)
    {
        using var binding = scope.BindForService(service);
        attempt.RequireScope(scope);
        var pending = await store.ListAsync(scope, cancellationToken).ConfigureAwait(false);
        scope.RequireCurrent(cancellationToken);
        if (!pending.Any(saved => saved.Same(attempt))) throw new CalendarAttemptConflictException();
        // Match the native clients' conservative one-day replay window, within server dedup retention.
        if (!attempt.CanRetry(Now)) throw new CalendarAttemptExpiredException();
        var receipt = await service.SendPreparedCalendarAsync(scope, attempt, cancellationToken).ConfigureAwait(false);
        scope.RequireCurrent();
        if (receipt.Kind != attempt.Kind || attempt.Kind == CalendarAttemptKind.Calendar && receipt.Calendar is null
            || attempt.Kind == CalendarAttemptKind.Event && receipt.Event is null) throw new PostboxException(502, "bad_response");
        try
        {
            await store.RemoveAsync(scope, attempt, cancellationToken).ConfigureAwait(false);
            scope.RequireCurrent(); return new(receipt, false);
        }
        catch (Exception)
        {
            scope.RequireCurrent(); return new(receipt, true);
        }
    }
    public async Task<bool> DiscardAsync(MessageRecoveryScope scope, PreparedCalendarAttempt expected, CancellationToken cancellationToken = default)
    {
        using var binding = scope.BindForService(service);
        var removed = await store.RemoveAsync(scope, expected, cancellationToken).ConfigureAwait(false);
        scope.RequireCurrent(cancellationToken); return removed;
    }
}
