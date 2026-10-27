using Pigeonpost.Core;
using Pigeonpost.Core.Calendar;

internal static partial class CalendarStoreTests
{
    public static IEnumerable<(string Name, Func<Task> Run)> Actions()
    {
        yield return ("A calendar save rechecks membership and sends the reviewed revision and occurrence scope", async () =>
        {
            using var f = new Fixture(); f.Source.Detail = (_, key) => Task.FromResult(Editable(key));
            await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
            var reviewed = (await f.Model.ReviewEditAsync(action, Key("a"), CalendarEventScope.This))!;
            var draft = CalendarEventDraft.From(reviewed, "UTC") with { Title = "Reviewed edit" }; var reads = f.Source.MailboxReads;
            var result = await f.Model.UpdateEventAsync(action, draft);
            Require(result is not null && f.Source.Updates == 1 && f.Source.LastRevision == 3 && f.Source.LastScope == CalendarEventScope.This
                && f.Source.LastKey == Key("a") && f.Source.LastMailbox == A && f.Source.LastFields!.Fields().GetProperty("title").GetString() == "Reviewed edit"
                && f.Source.MailboxReads > reads && !f.Model.State.Busy && !f.Model.State.RevisionConflict);
        });
        yield return ("A changed calendar revision refreshes detail without rebasing or sending the retained form", async () =>
        {
            using var f = new Fixture(); var revision = 3L; f.Source.Detail = (_, key) => Task.FromResult(Editable(key, revision));
            await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
            var reviewed = (await f.Model.ReviewEditAsync(action, Key("a"), CalendarEventScope.All))!;
            var draft = CalendarEventDraft.From(reviewed, "UTC") with { Title = "Unsaved form" }; revision = 4;
            Require(await f.Model.UpdateEventAsync(action, draft) is null && f.Source.Updates == 0 && f.Model.State.RevisionConflict
                && f.Model.State.Detail?.Event.Revision == 4 && draft.Target!.Revision == 3 && draft.Title == "Unsaved form" && !f.Model.State.Busy);
        });
        yield return ("A server calendar revision conflict preserves the form and refreshes the changed revision", async () =>
        {
            using var f = new Fixture(); var revision = 3L; f.Source.Detail = (_, key) => Task.FromResult(Editable(key, revision));
            f.Source.Update = (_, key) => { revision = 4; throw new PostboxException(409, "revision_conflict"); };
            await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
            var draft = CalendarEventDraft.From((await f.Model.ReviewEditAsync(action, Key("a"), CalendarEventScope.All))!, "UTC") with { Title = "Retained form" };
            Require(await f.Model.UpdateEventAsync(action, draft) is null && f.Source.Updates == 1 && f.Model.State.RevisionConflict
                && f.Model.State.Detail?.Event.Revision == 4 && draft.Title == "Retained form" && draft.Target!.Revision == 3 && !f.Model.State.Busy);
        });
        yield return ("Withdrawn organizer access erases calendar cache and cannot save a previously reviewed edit", async () =>
        {
            using var f = new Fixture(); f.Source.Detail = (_, key) => Task.FromResult(Editable(key));
            await f.Model.ActivateAsync(A, Features); var view = f.Model.CaptureView()!; using var action = view.BeginAction()!;
            var draft = CalendarEventDraft.From((await f.Model.ReviewEditAsync(action, Key("a"), CalendarEventScope.All))!, "UTC") with { Title = "Old authority" };
            f.Source.Detail = (_, key) => Task.FromResult(Editable(key) with { Event = Editable(key).Event with { My = new(Role: "attendee"), CanEdit = false } });
            Require(await f.Model.UpdateEventAsync(action, draft) is null && f.Source.Updates == 0 && !view.IsCurrent && !f.Model.State.Enabled && f.Model.State.Detail is null);
        });
        yield return ("An editor from a prior calendar inbox cannot be saved through the current inbox controls", async () =>
        {
            using var f = new Fixture(); f.Source.Detail = (_, key) => Task.FromResult(Editable(key));
            await f.Model.ActivateAsync(A, Features); using var old = f.Model.CaptureView()!.BeginAction()!;
            var draft = CalendarEventDraft.From((await f.Model.ReviewEditAsync(old, Key("a"), CalendarEventScope.All))!, "UTC") with { Title = "Prior inbox" };
            await f.Model.ActivateAsync(B, Features); using var current = f.Model.CaptureView()!.BeginAction()!; var reads = f.Source.DetailReads;
            Require(await f.Model.UpdateEventAsync(current, draft) is null && f.Source.DetailReads == reads && f.Source.Updates == 0 && f.Model.State.Mailbox == B);
        });
        yield return ("Calendar mutation retains both the original action caller and a later independent invocation caller", async () =>
        {
            using var f = new Fixture(); f.Source.Detail = (_, key) => Task.FromResult(Editable(key)); await f.Model.ActivateAsync(A, Features);
            var callerA = f.Source.Root.Child(); var callerB = f.Source.Root.Child(); CalendarActionTarget action;
            using (new AuthorizationContext(callerA)) action = f.Model.CaptureView()!.BeginAction()!; using var captured = action;
            var draft = CalendarEventDraft.From((await f.Model.ReviewEditAsync(action, Key("a"), CalendarEventScope.All))!, "UTC") with { Title = "Delayed form" };
            var waiting = Signal(); var release = Signal(); f.Source.Detail = async (_, key) => { waiting.TrySetResult(); await release.Task; return Editable(key); };
            Task<CalendarEventResponse?> save; using (new AuthorizationContext(callerB)) save = f.Model.UpdateEventAsync(action, draft);
            await waiting.Task.WaitAsync(TimeSpan.FromSeconds(3)); callerB.Retire(); release.TrySetResult();
            Require(await save is null && f.Source.Updates == 0 && callerA.IsActive && f.Source.Root.IsActive && action.IsCurrent && !f.Model.State.Busy);
        });
        yield return ("An unretired account replacement during a calendar write cannot publish or refresh under the next account", async () =>
        {
            using var f = new Fixture(); f.Source.Detail = (_, key) => Task.FromResult(Editable(key)); await f.Model.ActivateAsync(A, Features);
            using var action = f.Model.CaptureView()!.BeginAction()!;
            var draft = CalendarEventDraft.From((await f.Model.ReviewEditAsync(action, Key("a"), CalendarEventScope.All))!, "UTC") with { Title = "In flight" };
            var waiting = Signal(); var release = Signal(); f.Source.Update = async (_, key) => { waiting.TrySetResult(); await release.Task; return Editable(key); };
            var save = f.Model.UpdateEventAsync(action, draft); await waiting.Task.WaitAsync(TimeSpan.FromSeconds(3)); var reads = f.Source.MailboxReads;
            var original = f.Source.Root; f.Source.Root = new(); release.TrySetResult();
            Require(await save is null && f.Source.Updates == 1 && original.IsActive && !action.IsCurrent && !f.Model.State.Enabled && f.Model.State.Detail is null && f.Source.MailboxReads == reads);
            original.Retire();
        });
        yield return ("An unanswered calendar member can RSVP without changing contacts or unrelated inbox state", async () =>
        {
            using var f = new Fixture(); f.Source.Detail = (_, key) => Task.FromResult(Editable(key) with { Event = Editable(key).Event with { My = new(Role: "attendee", AttendeeId: "att_fixture"), CanEdit = false } });
            await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
            Require(await f.Model.AnswerAsync(action, Key("a"), CalendarAnswer.Tentative, CalendarAnswerScope.This, "Synthetic note")
                && f.Source.Answers == 1 && f.Source.LastMailbox == A && f.Source.LastKey == Key("a") && f.Source.LastAnswer == CalendarAnswer.Tentative && !f.Model.State.Busy);
        });
        yield return ("An unknown calendar role or absent attendee identity cannot submit an RSVP", async () =>
        {
            foreach (var member in new[] { new CalendarMembership(Role: "future_role", AttendeeId: "att_fixture"), new CalendarMembership(Role: "attendee") })
            {
                using var f = new Fixture(); f.Source.Detail = (_, key) => Task.FromResult(Editable(key) with { Event = Editable(key).Event with { My = member } });
                await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
                Require(!await f.Model.AnswerAsync(action, Key("a"), CalendarAnswer.Accepted, CalendarAnswerScope.All) && f.Source.Answers == 0 && !f.Model.State.Enabled);
            }
        });
        yield return ("Calendar cancellation retains its reviewed revision and scope instead of adopting a newer event", async () =>
        {
            using var f = new Fixture(); var revision = 3L; f.Source.Detail = (_, key) => Task.FromResult(Editable(key, revision));
            await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
            var target = (await f.Model.ReviewEditAsync(action, Key("a"), CalendarEventScope.Following))!;
            Require(await f.Model.CancelEventAsync(action, target) && f.Source.Cancels == 1 && f.Source.LastScope == CalendarEventScope.Following && f.Source.LastRevision == 3);
            revision = 4; Require(!await f.Model.CancelEventAsync(action, target) && f.Source.Cancels == 1 && f.Model.State.RevisionConflict);
        });
        yield return ("Calendar reminder and ICS actions verify fresh membership and mailbox ownership", async () =>
        {
            using var f = new Fixture(); f.Source.Detail = (_, key) => Task.FromResult(Editable(key)); await f.Model.ActivateAsync(A, Features);
            using var action = f.Model.CaptureView()!.BeginAction()!;
            Require(await f.Model.SaveRemindersAsync(action, Key("a"), [new(10, "push")]) && f.Source.ReminderSaves == 1);
            Require(System.Text.Encoding.UTF8.GetString((await f.Model.ExportAsync(action, Key("a")))!) == "BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n" && f.Source.Exports == 1);
            f.Source.Owned = [new(B)]; Require(await f.Model.ExportAsync(action, Key("a")) is null && f.Source.Exports == 1 && !f.Model.State.Enabled);
        });
        yield return ("An uncertain calendar edit keeps the submitted form without automatic retry", async () =>
        {
            using var f = new Fixture(); f.Source.Detail = (_, key) => Task.FromResult(Editable(key)); f.Source.Update = (_, _) => throw new HttpRequestException("Synthetic response loss.");
            await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
            var draft = CalendarEventDraft.From((await f.Model.ReviewEditAsync(action, Key("a"), CalendarEventScope.All))!, "UTC") with { Title = "Kept after loss" };
            Require(await f.Model.UpdateEventAsync(action, draft) is null && f.Source.Updates == 1 && draft.Title == "Kept after loss" && f.Model.State.Error?.Contains("not confirmed", StringComparison.Ordinal) == true && !f.Model.State.Busy);
            await f.Model.ReloadAsync(action); Require(f.Source.Updates == 1);
        });
        yield return ("Today follows the saved local zone across midnight while an explicitly selected day stays fixed", async () =>
        {
            using var f = new Fixture(); var clock = new Clock { Now = DateTimeOffset.Parse("2026-10-26T22:30:00Z") };
            using var model = new CalendarStore(f.Source, f.Source, clock: clock); await model.ActivateAsync(A, Features);
            using var action = model.CaptureView()!.BeginAction()!; Require(model.State.SelectedDay.Text == "2026-10-27" && model.State.FollowingToday);
            clock.Now = clock.Now.AddDays(1); await model.ReloadAsync(action); Require(model.State.SelectedDay.Text == "2026-10-28");
            Require(model.SelectDay(action, CalendarDay.Parse("2026-10-24"))); clock.Now = clock.Now.AddDays(1); await model.ReloadAsync(action);
            Require(model.State.SelectedDay.Text == "2026-10-24" && !model.State.FollowingToday);
            Require(model.Today(action) && model.State.SelectedDay.Text == "2026-10-29" && model.State.FollowingToday);
        });
    }
    private static CalendarEventResponse Editable(CalendarEventKey key, long revision = 3) => new(new(key.EventId, Revision: revision, Status: "confirmed", Title: "Original fixture",
        AllDay: false, Start: "2026-10-27T09:00", End: "2026-10-27T10:00", Tzid: "Europe/Istanbul", My: new(Role: "organizer"), CanEdit: true), new(OccurrenceId: key.OccurrenceId));
    private sealed partial class Source
    {
        internal int Updates, Cancels, Answers, ReminderSaves, Exports;
        internal string? LastMailbox; internal CalendarEventKey? LastKey; internal long LastRevision;
        internal CalendarEventScope LastScope; internal CalendarAnswer LastAnswer; internal CalendarWritePayload? LastFields;
        internal Func<string, CalendarEventKey, Task<CalendarEventResponse>>? Update;
        public Task<CalendarEventResponse> UpdateCalendarEventAsync(string identity, CalendarEventKey key, long expectedRevision, CalendarEventScope scope, CalendarWritePayload changes, CancellationToken ct)
        { ct.ThrowIfCancellationRequested(); Updates++; LastMailbox = identity; LastKey = key; LastRevision = expectedRevision; LastScope = scope; LastFields = changes; return Update?.Invoke(identity, key) ?? Task.FromResult(Editable(key, expectedRevision + 1)); }
        public Task CancelCalendarEventAsync(string identity, CalendarEventKey key, long expectedRevision, CalendarEventScope scope, string? message, CancellationToken ct)
        { ct.ThrowIfCancellationRequested(); Cancels++; LastMailbox = identity; LastKey = key; LastRevision = expectedRevision; LastScope = scope; return Task.CompletedTask; }
        public Task AnswerCalendarInvitationAsync(string identity, CalendarEventKey key, CalendarAnswer answer, CalendarAnswerScope scope, string? note, CancellationToken ct)
        { ct.ThrowIfCancellationRequested(); Answers++; LastMailbox = identity; LastKey = key; LastAnswer = answer; return Task.CompletedTask; }
        public Task SaveCalendarRemindersAsync(string identity, string eventId, IReadOnlyList<CalendarReminder> reminders, CancellationToken ct)
        { ct.ThrowIfCancellationRequested(); ReminderSaves++; LastMailbox = identity; return Task.CompletedTask; }
        public Task<CalendarSettings> SaveCalendarSettingsAsync(string identity, CalendarWritePayload fields, CancellationToken ct)
        { ct.ThrowIfCancellationRequested(); LastMailbox = identity; LastFields = fields; return Task.FromResult(new CalendarSettings(Timezone: "Europe/Istanbul", Previews: false)); }
        public Task<byte[]> ExportCalendarEventAsync(string identity, string eventId, CancellationToken ct)
        { ct.ThrowIfCancellationRequested(); Exports++; LastMailbox = identity; return Task.FromResult(System.Text.Encoding.UTF8.GetBytes("BEGIN:VCALENDAR\r\nEND:VCALENDAR\r\n")); }
    }
}
