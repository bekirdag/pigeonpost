using NodaTime;

namespace Pigeonpost.Core.Calendar;

// Rendering and action authority are separate. Each actual gesture captures all its callers.
public sealed class CalendarContextView(Func<bool> current, Func<CalendarActionTarget?> begin)
{
    private int active = 1;
    public bool IsCurrent => Volatile.Read(ref active) != 0 && current();
    public CalendarActionTarget? BeginAction() => IsCurrent ? begin() : null;
    public void Retire() => Interlocked.Exchange(ref active, 0);
}
public sealed class CalendarActionTarget : IDisposable
{
    internal CalendarStore Owner { get; }
    internal object Actor { get; }
    private readonly AuthorizationLease authority;
    private readonly Func<bool> current;
    private readonly Func<AuthorizationContext> bind;
    internal CalendarActionTarget(CalendarStore owner, object actor, AuthorizationLease authority, Func<bool> current, Func<AuthorizationContext> bind)
    { Owner = owner; Actor = actor; this.authority = authority; this.current = current; this.bind = bind; }
    public bool IsCurrent => authority.IsActive && current();
    internal AuthorizationContext Bind()
    {
        if (!IsCurrent) throw new AuthorizationRetiredException();
        var context = bind();
        try { if (!IsCurrent) throw new AuthorizationRetiredException(); return context; } catch { context.Dispose(); throw; }
    }
    public void Dispose() => authority.Retire();
    public override string ToString() => "CalendarActionTarget(redacted)";
}

public sealed partial class CalendarStore : ObservableObject, IDisposable
{
    private readonly ICalendarService api;
    private readonly IInboxService inbox;
    private readonly CalendarDeliveryCoordinator? delivery;
    private readonly TimeProvider clock;
    private sealed class Actor(AuthorizationLease accepted, string mailbox)
    {
        internal readonly Guid Id = Guid.NewGuid();
        internal readonly AuthorizationLease Accepted = accepted, Authority = accepted.Child();
        internal readonly string Mailbox = mailbox;
    }
    private Actor? actor;
    private CalendarState state;
    private int loadVersion, detailVersion, pendingVersion;
    private bool disposed;
    public CalendarStore(ICalendarService api, IInboxService inbox, CalendarDeliveryCoordinator? delivery = null, TimeProvider? clock = null)
    {
        if (!ReferenceEquals(api, inbox)) throw new ArgumentException("Calendar and inbox access must use the same accepted service.");
        this.api = api; this.inbox = inbox; this.delivery = delivery; this.clock = clock ?? TimeProvider.System; state = Empty();
    }
    private CalendarState Empty(string? error = null) => new(new CalendarDay(DateOnly.FromDateTime(clock.GetUtcNow().UtcDateTime))) { Error = error };
    public CalendarState State => actor is { } current && Current(current) ? state : Empty(state.Error);
    private bool Current(Actor owner) => !disposed && ReferenceEquals(actor, owner) && owner.Authority.IsActive
        && ReferenceEquals(api.AcceptedAuthorization, owner.Accepted) && ReferenceEquals(inbox.AcceptedAuthorization, owner.Accepted);
    private void Publish() => Changed(nameof(State));
    public CalendarContextView? CaptureView()
    {
        if (actor is not { } owner || !Current(owner) || !state.Enabled) return null;
        return new(() => Current(owner), () => CaptureAction(owner));
    }
    private CalendarActionTarget? CaptureAction(Actor owner)
    {
        if (!Current(owner) || !state.Enabled) return null;
        AuthorizationLease? authority = null;
        try
        {
            using var account = AuthorizationContext.BindActor(owner.Accepted, owner.Authority);
            authority = owner.Authority.Child(); using var local = new AuthorizationContext(authority);
            var validate = AuthorizationContext.CaptureValidation(() => api.AcceptedAuthorization);
            bool StillCurrent() { try { validate(); return Current(owner); } catch (AuthorizationRetiredException) { return false; } }
            return new(this, owner, authority, StillCurrent, AuthorizationContext.CaptureBinding());
        }
        catch (AuthorizationRetiredException) { authority?.Retire(); return null; }
    }
    private sealed class Operation : IDisposable
    {
        internal readonly Actor Owner;
        private readonly CalendarStore model;
        private readonly CalendarActionTarget target;
        private readonly AuthorizationContext context;
        private readonly Action validate;
        private readonly Action<Action> write;
        internal Operation(CalendarStore model, Actor owner, CalendarActionTarget target)
        {
            this.model = model; Owner = owner; this.target = target; context = target.Bind();
            try { validate = AuthorizationContext.CaptureValidation(() => model.api.AcceptedAuthorization); write = AuthorizationContext.CaptureWriteBarrier(() => model.api.AcceptedAuthorization); Check(); }
            catch { context.Dispose(); throw; }
        }
        internal void Check()
        { validate(); if (!target.IsCurrent || !model.Current(Owner)) throw new AuthorizationRetiredException(); }
        internal bool IsCurrent { get { try { Check(); return true; } catch (AuthorizationRetiredException) { return false; } } }
        internal void Local(Action action) { Check(); write(() => { Check(); action(); }); Check(); }
        internal void RetiringLocal(Action action) { Check(); write(() => { Check(); action(); }); }
        public void Dispose() => context.Dispose();
    }
    private Operation? Begin(CalendarActionTarget target)
    {
        if (!ReferenceEquals(target.Owner, this) || actor is not { } owner || !ReferenceEquals(target.Actor, owner) || !target.IsCurrent) return null;
        try { return new(this, owner, target); } catch (AuthorizationRetiredException) { return null; }
    }
    private async Task VerifyInbox(Operation operation, CancellationToken ct)
    {
        var mailboxes = await inbox.GetMailboxesAsync(ct); operation.Check();
        if (!mailboxes.Any(m => m.Address == operation.Owner.Mailbox)) throw new MessageRecoveryNotAuthorizedException();
    }
    public async Task ActivateAsync(string? mailbox, ServerFeatures features, CancellationToken ct = default)
    {
        if (disposed) return;
        try
        {
            AuthorizationContext.CaptureValidation(() => api.AcceptedAuthorization)();
            var accepted = api.AcceptedAuthorization;
            if (accepted is null || !ReferenceEquals(accepted, inbox.AcceptedAuthorization)) { Clear(); return; }
            using var context = new AuthorizationContext(accepted);
            AuthorizationContext.CaptureValidation(() => api.AcceptedAuthorization)();
            Clear();
            if (mailbox is null || !features.Has(ServerFeatures.Calendar)) return;
            if (!MessageRecoveryScope.CanonicalMailbox(mailbox)) throw new ArgumentException("Choose a canonical calendar inbox.");
            var owner = new Actor(accepted, mailbox); actor = owner;
            state = Empty() with { ContextId = owner.Id, Mailbox = mailbox, Enabled = true, EmailEnabled = features.Has(ServerFeatures.CalendarEmail) }; Publish();
            using var target = CaptureAction(owner);
            if (target is null) return;
            await ReloadAsync(target, ct);
            if (target.IsCurrent) await RefreshPendingAsync(target, ct);
        }
        catch (OperationCanceledException) { }
    }
    public void Clear()
    {
        if (disposed) return;
        try { AuthorizationContext.CaptureValidation(() => api.AcceptedAuthorization)(); }
        catch (AuthorizationRetiredException) { return; }
        actor?.Authority.Retire(); actor = null; loadVersion++; detailVersion++; pendingVersion++; activity = null; state = Empty(); Publish();
    }
    public bool SelectDay(CalendarActionTarget target, CalendarDay day)
    {
        using var operation = Begin(target); if (operation is null) return false;
        operation.Local(() => { loadVersion++; detailVersion++; state = state with { SelectedDay = day, OpenEvent = null, Detail = null }; }); Publish(); return true;
    }
    public bool SelectCalendars(CalendarActionTarget target, IEnumerable<string>? selected)
    {
        var ids = selected is null ? null : new HashSet<string>(selected, StringComparer.Ordinal);
        if (ids?.Any(id => id.Length is < 1 or > 128 || id.Any(c => !char.IsAsciiLetterOrDigit(c) && c is not '_' and not '-')) == true) return false;
        using var operation = Begin(target); if (operation is null) return false;
        operation.Local(() => { loadVersion++; state = state with { SelectedCalendars = ids }; }); Publish(); return true;
    }
    public async Task ReloadAsync(CalendarActionTarget target, CancellationToken ct = default)
    {
        using var operation = Begin(target); if (operation is null) return;
        var request = ++loadVersion; var day = state.SelectedDay; var selected = state.SelectedCalendars?.ToHashSet(StringComparer.Ordinal);
        var detailKey = state.OpenEvent; var detailRequest = detailKey is null ? detailVersion : ++detailVersion;
        operation.Local(() => state = state with { Loading = true, Error = null }); Publish();
        try
        {
            await VerifyInbox(operation, ct);
            var foldersTask = api.GetCalendarsAsync(operation.Owner.Mailbox, ct);
            var settingsTask = api.GetCalendarSettingsAsync(operation.Owner.Mailbox, ct);
            var invitationsTask = api.GetCalendarInvitationsAsync(operation.Owner.Mailbox, false, ct);
            var filteredTask = api.GetCalendarInvitationsAsync(operation.Owner.Mailbox, true, ct);
            await Task.WhenAll(foldersTask, settingsTask, invitationsTask, filteredTask); operation.Check();
            if (request != loadVersion) return;
            var settings = settingsTask.Result;
            string zone; string? warning = null;
            try { zone = CalendarTime.Zone(settings.Timezone ?? "UTC").Id; }
            catch (CalendarTimeException) { zone = "UTC"; warning = "The saved time zone is unavailable. Times are shown in UTC."; }
            selected?.IntersectWith(foldersTask.Result.Select(f => f.Id));
            var range = new CalendarMonthWindow(day, settings.WeekStart is >= 0 and <= 6 ? settings.WeekStart.Value : 1).Interval(CalendarTime.Zone(zone));
            // An explicit empty filter is none. The wire API's empty selection means all.
            var rows = selected is { Count: 0 } ? [] : await api.GetCalendarOccurrencesAsync(operation.Owner.Mailbox, range.Start, range.End, selected?.ToArray(), ct);
            CalendarEventResponse? detail = null; var removed = false;
            if (detailKey is not null)
            {
                try
                {
                    detail = await api.GetCalendarEventAsync(operation.Owner.Mailbox, detailKey, ct);
                    if (detail.Event?.Id != detailKey.EventId || detailKey.OccurrenceId is not null && detail.Occurrence?.OccurrenceId != detailKey.OccurrenceId)
                        throw new PostboxException(502, "bad_response");
                }
                catch (PostboxException error) when (error.StatusCode == 404 && error.Code != "feature_disabled") { removed = true; }
            }
            operation.Check(); if (request != loadVersion) return;
            var stillSelected = detailRequest == detailVersion && state.OpenEvent == detailKey;
            if (removed && stillSelected) rows = rows.Where(o => o.EventId != detailKey!.EventId).ToArray();
            operation.Local(() => state = state with { Folders = foldersTask.Result.ToArray(), Settings = settings, DisplayZone = zone, SelectedCalendars = selected,
                Occurrences = rows.ToArray(), Invitations = invitationsTask.Result.Where(i => !removed || !stillSelected || i.EventId != detailKey!.EventId).ToArray(),
                FilteredInvitations = filteredTask.Result.Where(i => !removed || !stillSelected || i.EventId != detailKey!.EventId).ToArray(),
                OpenEvent = removed && stillSelected ? null : state.OpenEvent, Detail = stillSelected ? detail : state.Detail, Error = warning }); Publish();
        }
        catch (OperationCanceledException) { }
        catch (Exception error) { if (request == loadVersion) Fail(operation, error); }
        finally
        {
            if (Current(operation.Owner) && request == loadVersion)
            { operation.Owner.Authority.WithActive(() => { state = state with { Loading = false }; return true; }); Publish(); }
        }
    }
    public async Task<CalendarEventResponse?> OpenEventAsync(CalendarActionTarget target, CalendarEventKey key, CancellationToken ct = default)
    {
        using var operation = Begin(target); if (operation is null) return null;
        var request = ++detailVersion;
        operation.Local(() => state = state with { OpenEvent = key, Detail = null, Error = null }); Publish();
        try
        {
            await VerifyInbox(operation, ct);
            var result = await api.GetCalendarEventAsync(operation.Owner.Mailbox, key, ct); operation.Check();
            if (request != detailVersion || state.OpenEvent != key) return null;
            if (result.Event?.Id != key.EventId || key.OccurrenceId is not null && result.Occurrence?.OccurrenceId != key.OccurrenceId) throw new PostboxException(502, "bad_response");
            operation.Local(() => state = state with { Detail = result }); Publish(); return result;
        }
        catch (OperationCanceledException) { return null; }
        catch (PostboxException error) when (error.StatusCode == 404 && error.Code != "feature_disabled")
        {
            if (operation.IsCurrent && request == detailVersion)
            {
                operation.Local(() => state = state with { Detail = null, OpenEvent = null, Occurrences = state.Occurrences.Where(o => o.EventId != key.EventId).ToArray(),
                    Invitations = state.Invitations.Where(i => i.EventId != key.EventId).ToArray(), FilteredInvitations = state.FilteredInvitations.Where(i => i.EventId != key.EventId).ToArray(), Error = "This calendar event is no longer available." }); Publish();
            }
            return null;
        }
        catch (Exception error) { if (request == detailVersion) Fail(operation, error); return null; }
    }
    public bool CloseEvent(CalendarActionTarget target)
    {
        using var operation = Begin(target); if (operation is null) return false;
        operation.Local(() => { detailVersion++; state = state with { OpenEvent = null, Detail = null }; }); Publish(); return true;
    }
    public async Task RefreshPendingAsync(CalendarActionTarget target, CancellationToken ct = default)
    {
        using var operation = Begin(target); if (operation is null || delivery is null) return;
        var request = ++pendingVersion;
        try
        {
            using var scope = await MessageRecoveryScope.AuthorizeAsync(inbox, operation.Owner.Mailbox, ct); operation.Check();
            if (scope is null)
            { operation.Local(() => state = state with { AttemptsLoaded = false, DurableWrites = false, Pending = [] }); Publish(); return; }
            var pending = await delivery.GetPendingAsync(scope, ct); operation.Check(); if (request != pendingVersion) return;
            operation.Local(() => state = state with { Pending = pending.ToArray(), AttemptsLoaded = true, AttemptsUnreadable = false, DurableWrites = true }); Publish();
        }
        catch (OperationCanceledException) { }
        catch (Exception error)
        {
            if (!operation.IsCurrent || request != pendingVersion) return;
            if (error is MessageRecoveryNotAuthorizedException || error is PostboxException denied && (denied.StatusCode is 401 or 403 || denied.Code == "feature_disabled"))
            { Fail(operation, error); return; }
            operation.Local(() => state = state with { AttemptsLoaded = true, AttemptsUnreadable = true, DurableWrites = false, Error = Describe(error) }); Publish();
        }
    }
    private void Fail(Operation operation, Exception error)
    {
        if (!operation.IsCurrent) return;
        if (error is MessageRecoveryNotAuthorizedException || error is PostboxException postbox && (postbox.StatusCode is 401 or 403 || postbox.Code == "feature_disabled"))
        {
            var owner = operation.Owner;
            operation.RetiringLocal(() => { state = Empty(Describe(error)); actor = null; loadVersion++; detailVersion++; pendingVersion++; activity = null; });
            owner.Authority.Retire(); Publish(); return;
        }
        operation.Local(() => state = state with { Detail = null, Error = Describe(error) }); Publish();
    }
    private static string Describe(Exception error) => error is PostboxException or CalendarAttemptUnavailableException or CalendarAttemptConflictException
        or CalendarAttemptExpiredException ? error.Message : error is MessageRecoveryNotAuthorizedException ? "Access to this calendar inbox changed. Open it again."
        : "Could not refresh the calendar. Try again.";
    public void Dispose()
    { if (disposed) return; disposed = true; actor?.Authority.Retire(); actor = null; activity = null; state = Empty(); }
}
