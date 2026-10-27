using System.Text;

namespace Pigeonpost.Core.Calendar;

public static class CalendarMeetingCode
{
    public static string? Parse(string input)
    {
        if (input.Length is not (10 or 11) || input.Length == 11 && input[5] != '-') return null;
        var code = input.Length == 11 ? input.Remove(5, 1).ToUpperInvariant() : input.ToUpperInvariant();
        return code.Length == 10 && code.All(c => "ABCDEFGHJKMNPQRSTUVWXYZ23456789".Contains(c)) ? code : null;
    }
}

// A member intent, not a room role or credential. Only CalendarStore can prepare one.
public sealed class CalendarScheduledJoin : IDisposable
{
    private readonly AuthorizationLease accepted, grant;
    private readonly Action validate;
    private readonly Func<AuthorizationContext> bind;
    public string Mailbox { get; }
    public string Code { get; }
    public string EventId { get; }
    public string OccurrenceId { get; }
    public string Title { get; }
    public bool Video { get; }
    internal CalendarScheduledJoin(string mailbox, string code, CalendarEventKey key, string title, bool video,
        AuthorizationLease accepted, AuthorizationLease grant, Action validate, Func<AuthorizationContext> bind)
    {
        Mailbox = mailbox; Code = code; EventId = key.EventId; OccurrenceId = key.OccurrenceId!;
        Title = string.Concat(title.EnumerateRunes().Take(200).Select(r => r.ToString())); Video = video;
        this.accepted = accepted; this.grant = grant; this.validate = validate; this.bind = bind;
    }
    public void RequireCurrent(AuthorizationLease? currentAccount, string? currentMailbox)
    {
        RequirePreparedCurrent();
        if (!ReferenceEquals(currentAccount, accepted) || currentMailbox != Mailbox) throw new AuthorizationRetiredException();
    }
    internal void RequirePreparedCurrent() { grant.RequireActive(); validate(); }
    public CalendarJoinBinding BindForService(IAccessTokenProvider provider, string currentMailbox)
    {
        var context = bind();
        try
        {
            RequireCurrent(provider.AcceptedAuthorization, currentMailbox);
            var validation = AuthorizationContext.CaptureValidation(provider); validation();
            RequireCurrent(provider.AcceptedAuthorization, currentMailbox);
            return new(this, provider, currentMailbox, context, validation);
        }
        catch { context.Dispose(); throw; }
    }
    public void Dispose() => grant.Retire();
    public override string ToString() => "CalendarScheduledJoin(redacted)";
}

public sealed class CalendarJoinBinding : IDisposable
{
    private readonly CalendarScheduledJoin request;
    private readonly IAccessTokenProvider provider;
    private readonly string mailbox;
    private readonly AuthorizationContext context;
    private readonly Action validate;
    private bool disposed;
    internal CalendarJoinBinding(CalendarScheduledJoin request, IAccessTokenProvider provider, string mailbox, AuthorizationContext context, Action validate)
    { this.request = request; this.provider = provider; this.mailbox = mailbox; this.context = context; this.validate = validate; }
    public void RequireCurrent()
    { if (disposed) throw new AuthorizationRetiredException(); validate(); request.RequireCurrent(provider.AcceptedAuthorization, mailbox); }
    public void Dispose() { if (disposed) return; disposed = true; context.Dispose(); }
    public override string ToString() => "CalendarJoinBinding(redacted)";
}

public sealed partial class CalendarStore
{
    public async Task<CalendarScheduledJoin?> PrepareScheduledJoinAsync(CalendarActionTarget target, CalendarEventKey key,
        string reviewedCode, bool video, Guid expectedContext, CancellationToken ct = default)
    {
        using var operation = Begin(target); if (operation is null || expectedContext != state.ContextId || state.Busy) return null;
        try
        {
            var code = CalendarMeetingCode.Parse(reviewedCode);
            if (code is null || key.OccurrenceId is not { Length: > 0 and <= 256 } || key.OccurrenceId.Any(char.IsControl)) throw new ArgumentException("Choose a specific meeting occurrence from the agenda.");
            await VerifyInbox(operation, ct);
            var detail = await FreshDetail(operation, key, ct); ct.ThrowIfCancellationRequested(); operation.Check();
            var e = detail.Event; var o = detail.Occurrence;
            if (e.Status != "confirmed" || o?.Status is not (null or "confirmed") || (o?.AllDay ?? e.AllDay) != false)
                throw new ArgumentException("This event has no available scheduled call.");
            if (e.My?.Role is not ("organizer" or "attendee" or "cohost") || (o?.MyRsvp ?? e.My.Rsvp) == "declined"
                || e.My.Role is "attendee" or "cohost" && string.IsNullOrEmpty(e.My.AttendeeId))
                throw new ArgumentException("This inbox cannot join the meeting. Review its invitation first.");
            if (e.Meeting is not { Enabled: not false } meeting || CalendarMeetingCode.Parse(meeting.Code ?? "") != code)
                throw new ArgumentException("The meeting changed. Refresh the event and select it again.");
            var grant = operation.Owner.Authority.Child();
            try
            {
                using var admitted = new AuthorizationContext(grant);
                return new(operation.Owner.Mailbox, code, key, o?.Title ?? e.Title ?? "Meeting", video, operation.Owner.Accepted, grant,
                    AuthorizationContext.CaptureValidation(() => api.AcceptedAuthorization), AuthorizationContext.CaptureBinding());
            }
            catch { grant.Retire(); throw; }
        }
        catch (OperationCanceledException) { return null; }
        catch (ArgumentException error)
        { if (operation.IsCurrent) { operation.Local(() => state = state with { Error = error.Message }); Publish(); } return null; }
        catch (Exception error) { Fail(operation, error); return null; }
    }
}
