namespace Pigeonpost.Core.Calendar;

public sealed partial class CalendarStore
{
    private sealed class RevisionChangedException : Exception;
    private async Task<CalendarEventResponse> FreshDetail(Operation operation, CalendarEventKey key, CancellationToken ct)
    {
        var detail = await api.GetCalendarEventAsync(operation.Owner.Mailbox, key, ct); operation.Check();
        if (detail.Event?.Id != key.EventId || key.OccurrenceId is not null && detail.Occurrence?.OccurrenceId != key.OccurrenceId) throw new PostboxException(502, "bad_response");
        return detail;
    }
    private static bool Organizer(CalendarEventResponse detail) => detail.Event.My?.Role == "organizer" && detail.Event.CanEdit == true && detail.Event.Status == "confirmed";
    private static bool Member(CalendarEventResponse detail) => detail.Event.My?.Role is "organizer" or "attendee" or "cohost";
    public async Task<CalendarEditTarget?> ReviewEditAsync(CalendarActionTarget target, CalendarEventKey key, CalendarEventScope scope, CancellationToken ct = default)
    {
        var detail = await OpenEventAsync(target, key, ct);
        if (detail is null || !target.IsCurrent || !Organizer(detail)) return null;
        try { return new(detail, key, scope, state.ContextId); } catch (ArgumentException) { return null; }
    }
    private async Task<T?> Mutate<T>(CalendarActionTarget target, CalendarEventKey? key,
        Func<Operation, ICalendarActionsService, Task<T>> work, CancellationToken ct) where T : class
    {
        using var operation = Begin(target);
        if (operation is null || api is not ICalendarActionsService actions || state.Busy) return null;
        var token = StartActivity(operation);
        try
        {
            await VerifyInbox(operation, ct);
            var result = await work(operation, actions); operation.Check();
            await ReloadAsync(target, ct); operation.Check(); return result;
        }
        catch (OperationCanceledException) { return null; }
        catch (Exception error) when (error is RevisionChangedException || error is PostboxException { Code: "revision_conflict" })
        {
            if (!operation.IsCurrent) return null;
            if (key is not null) await OpenEventAsync(target, key, ct);
            if (operation.IsCurrent)
            { operation.Local(() => state = state with { RevisionConflict = true, Error = "This event changed. Your form is kept. Review the latest version before saving again." }); Publish(); }
            return null;
        }
        catch (PostboxException error) when (error.StatusCode == 404 && error.Code != "feature_disabled" && key is not null)
        {
            if (operation.IsCurrent) await OpenEventAsync(target, key, ct);
            return null;
        }
        catch (Exception error)
        {
            if (error is HttpRequestException && operation.IsCurrent)
            { operation.Local(() => state = state with { Error = "The change was not confirmed. Your form is kept. Refresh the event before trying again." }); Publish(); }
            else Fail(operation, error);
            return null;
        }
        finally { operation.Dispose(); FinishActivity(operation.Owner, token); }
    }
    public Task<CalendarEventResponse?> UpdateEventAsync(CalendarActionTarget target, CalendarEventDraft draft, CancellationToken ct = default)
    {
        if (!target.IsCurrent || draft.Target is not { } reviewed || reviewed.ContextId != state.ContextId || draft.ContextId != state.ContextId)
            return Task.FromResult<CalendarEventResponse?>(null);
        var fields = draft.Fields(); if (!fields.Fields().EnumerateObject().Any()) throw new ArgumentException("There are no changes to save.");
        return Mutate(target, reviewed.Key, async (operation, actions) =>
        {
            var fresh = await FreshDetail(operation, reviewed.Key, ct);
            if (!Organizer(fresh)) throw new PostboxException(403, "forbidden");
            if (fresh.Event.Revision != reviewed.Revision) throw new RevisionChangedException();
            var updated = await actions.UpdateCalendarEventAsync(operation.Owner.Mailbox, reviewed.Key, reviewed.Revision, reviewed.Scope, fields, ct); operation.Check(); return updated;
        }, ct);
    }
    public async Task<bool> CancelEventAsync(CalendarActionTarget target, CalendarEditTarget reviewed, string? message = null, CancellationToken ct = default)
    {
        if (!target.IsCurrent || reviewed.ContextId != state.ContextId) return false;
        var result = await Mutate(target, reviewed.Key, async (operation, actions) =>
        {
            var fresh = await FreshDetail(operation, reviewed.Key, ct);
            if (!Organizer(fresh)) throw new PostboxException(403, "forbidden");
            if (fresh.Event.Revision != reviewed.Revision) throw new RevisionChangedException();
            await actions.CancelCalendarEventAsync(operation.Owner.Mailbox, reviewed.Key, reviewed.Revision, reviewed.Scope, message, ct); operation.Check(); return "cancelled";
        }, ct);
        return result is not null;
    }
    public async Task<bool> AnswerAsync(CalendarActionTarget target, CalendarEventKey key, CalendarAnswer answer, CalendarAnswerScope scope, string? note = null, CancellationToken ct = default)
    {
        var result = await Mutate(target, key, async (operation, actions) =>
        {
            var fresh = await FreshDetail(operation, key, ct);
            if (fresh.Event.My?.Role is not ("attendee" or "cohost") || string.IsNullOrEmpty(fresh.Event.My.AttendeeId) || fresh.Event.Status != "confirmed") throw new PostboxException(403, "forbidden");
            await actions.AnswerCalendarInvitationAsync(operation.Owner.Mailbox, key, answer, scope, note, ct); operation.Check(); return "answered";
        }, ct);
        return result is not null;
    }
    public async Task<bool> SaveRemindersAsync(CalendarActionTarget target, CalendarEventKey key, IReadOnlyList<CalendarReminder> reminders, CancellationToken ct = default)
    {
        var frozen = reminders.ToArray();
        var result = await Mutate(target, key, async (operation, actions) =>
        {
            if (!Member(await FreshDetail(operation, key, ct))) throw new PostboxException(403, "forbidden");
            await actions.SaveCalendarRemindersAsync(operation.Owner.Mailbox, key.EventId, frozen, ct); operation.Check(); return "saved";
        }, ct);
        return result is not null;
    }
    public Task<CalendarSettings?> SaveSettingsAsync(CalendarActionTarget target, CalendarWritePayload fields, CancellationToken ct = default) =>
        Mutate(target, null, async (operation, actions) => { var result = await actions.SaveCalendarSettingsAsync(operation.Owner.Mailbox, fields, ct); operation.Check(); return result; }, ct);
    public Task<byte[]?> ExportAsync(CalendarActionTarget target, CalendarEventKey key, CancellationToken ct = default) =>
        Mutate(target, key, async (operation, actions) =>
        {
            if (!Member(await FreshDetail(operation, key, ct))) throw new PostboxException(403, "forbidden");
            var bytes = await actions.ExportCalendarEventAsync(operation.Owner.Mailbox, key.EventId, ct); operation.Check(); return bytes;
        }, ct);
}
