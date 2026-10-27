using Pigeonpost.Core;
using Pigeonpost.Core.Calendar;

internal static class CalendarStoreTests
{
    private const string A = "/k/aaaaaaaaaaaaaaaaaaaaaaaaaa", B = "/k/bbbbbbbbbbbbbbbbbbbbbbbbbb";
    private static readonly ServerFeatures Features = new(new HashSet<string> { ServerFeatures.Calendar });
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Calendar activation scopes metadata and agenda to a fresh owned canonical inbox", async () =>
        {
            using var f = new Fixture(); await f.Model.ActivateAsync(A, Features);
            Require(f.Model.State.Enabled && f.Model.State.Mailbox == A && f.Model.State.Folders.Single().Id == "cal_a"
                && f.Model.State.Occurrences.Single().Title == "A event" && f.Model.State.DisplayZone == "Europe/Istanbul"
                && f.Model.State.Agenda.Count == 1 && !f.Model.State.CanCreate && !f.Model.State.Loading && f.Source.MailboxReads > 0);
            Require(f.Model.State.Invitations.Single().Title == "Pending A" && f.Model.State.FilteredInvitations.Single().Title == "Filtered A");
        });
        yield return ("Calendar feature-off activation erases the previous state without any optional route request", async () =>
        {
            using var f = new Fixture(); await f.Model.ActivateAsync(A, Features); var view = f.Model.CaptureView()!; var reads = f.Source.CalendarReads;
            await f.Model.ActivateAsync(A, ServerFeatures.None);
            Require(!view.IsCurrent && view.BeginAction() is null && !f.Model.State.Enabled && f.Model.State.Occurrences.Count == 0 && f.Source.CalendarReads == reads);
        });
        yield return ("An empty calendar selection stays none and never becomes the wire default of all calendars", async () =>
        {
            using var f = new Fixture(); await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
            var reads = f.Source.OccurrenceReads; Require(f.Model.SelectCalendars(action, [])); await f.Model.ReloadAsync(action);
            Require(f.Model.State.SelectedCalendars is { Count: 0 } && f.Model.State.Occurrences.Count == 0 && f.Source.OccurrenceReads == reads);
            Require(f.Model.SelectCalendars(action, null)); await f.Model.ReloadAsync(action); Require(f.Model.State.Occurrences.Count == 1 && f.Source.OccurrenceReads == reads + 1);
        });
        yield return ("Deleting the last selected calendar preserves an explicit empty filter", async () =>
        {
            using var f = new Fixture(); await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
            Require(f.Model.SelectCalendars(action, ["cal_a"])); var reads = f.Source.OccurrenceReads;
            f.Source.Folders = _ => Task.FromResult<IReadOnlyList<CalendarFolder>>([]); await f.Model.ReloadAsync(action);
            Require(f.Model.State.SelectedCalendars is { Count: 0 } && f.Model.State.Occurrences.Count == 0 && f.Source.OccurrenceReads == reads);
        });
        yield return ("An unretired calendar account replacement hides cached data and invalidates rendered controls", async () =>
        {
            using var f = new Fixture(); await f.Model.ActivateAsync(A, Features); var view = f.Model.CaptureView()!; var original = f.Source.Root;
            f.Source.Root = new(); Require(original.IsActive && !view.IsCurrent && view.BeginAction() is null && !f.Model.State.Enabled && f.Model.State.Folders.Count == 0);
            await f.Model.ActivateAsync(B, Features); Require(f.Model.State.Mailbox == B && f.Model.State.Occurrences.Single().Title == "B event"); original.Retire();
        });
        yield return ("A late calendar mailbox load cannot overwrite the newly selected inbox", async () =>
        {
            using var f = new Fixture(); var waiting = Signal(); var release = Signal();
            f.Source.Folders = async mailbox => { if (mailbox == A) { waiting.TrySetResult(); await release.Task; } return [new("cal_" + Name(mailbox))]; };
            var old = f.Model.ActivateAsync(A, Features); await waiting.Task.WaitAsync(TimeSpan.FromSeconds(3));
            await f.Model.ActivateAsync(B, Features); var context = f.Model.State.ContextId; release.TrySetResult(); await old;
            Require(f.Model.State.ContextId == context && f.Model.State.Mailbox == B && f.Model.State.Folders.Single().Id == "cal_b" && !f.Model.State.Loading);
        });
        yield return ("A calendar detail action retains its independent original caller after token or response delay", async () =>
        {
            using var f = new Fixture(); await f.Model.ActivateAsync(A, Features); var caller = f.Source.Root.Child(); CalendarActionTarget action;
            using (new AuthorizationContext(caller)) action = f.Model.CaptureView()!.BeginAction()!; using var captured = action;
            var waiting = Signal(); var release = Signal(); f.Source.Detail = async (_, key) => { waiting.TrySetResult(); await release.Task; return Detail(key); };
            var read = f.Model.OpenEventAsync(action, Key("a")); await waiting.Task.WaitAsync(TimeSpan.FromSeconds(3)); caller.Retire(); release.TrySetResult();
            Require(await read is null && f.Source.Root.IsActive && !action.IsCurrent && f.Model.State.Detail is null);
        });
        yield return ("A queued calendar gesture cannot borrow fresh authority from its still-rendered view", async () =>
        {
            using var f = new Fixture(); await f.Model.ActivateAsync(A, Features); var view = f.Model.CaptureView()!; var caller = f.Source.Root.Child(); CalendarActionTarget action;
            using (new AuthorizationContext(caller)) action = view.BeginAction()!; using var captured = action; caller.Retire();
            var reads = f.Source.DetailReads; Require(await f.Model.OpenEventAsync(action, Key("a")) is null && f.Source.DetailReads == reads && view.IsCurrent);
            using var fresh = view.BeginAction()!; Require(await f.Model.OpenEventAsync(fresh, Key("a")) is not null);
        });
        yield return ("An older calendar detail cannot replace the later selected event", async () =>
        {
            using var f = new Fixture(); await f.Model.ActivateAsync(A, Features); using var first = f.Model.CaptureView()!.BeginAction()!; using var second = f.Model.CaptureView()!.BeginAction()!;
            var waiting = Signal(); var release = Signal(); f.Source.Detail = async (_, key) => { if (key.EventId == "evt_a") { waiting.TrySetResult(); await release.Task; } return Detail(key); };
            var old = f.Model.OpenEventAsync(first, Key("a")); await waiting.Task.WaitAsync(TimeSpan.FromSeconds(3));
            Require(await f.Model.OpenEventAsync(second, Key("next")) is not null); release.TrySetResult(); Require(await old is null && f.Model.State.Detail?.Event.Id == "evt_next");
        });
        yield return ("Calendar access denial erases cached event data and retires all prior controls", async () =>
        {
            using var f = new Fixture(); await f.Model.ActivateAsync(A, Features); var view = f.Model.CaptureView()!; using var action = view.BeginAction()!;
            Require(await f.Model.OpenEventAsync(action, Key("a")) is not null);
            f.Source.Detail = (_, _) => throw new PostboxException(403, "forbidden");
            Require(await f.Model.OpenEventAsync(action, Key("a")) is null && !view.IsCurrent && !f.Model.State.Enabled
                && f.Model.State.Detail is null && f.Model.State.Occurrences.Count == 0 && f.Model.State.Invitations.Count == 0);
        });
        yield return ("A deleted calendar event is removed from details, agenda and both invitation projections", async () =>
        {
            using var f = new Fixture(); await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
            Require(await f.Model.OpenEventAsync(action, Key("a")) is not null);
            f.Source.Detail = (_, _) => throw new PostboxException(404, "not_found"); await f.Model.OpenEventAsync(action, Key("a"));
            Require(f.Model.State.Enabled && f.Model.State.Detail is null && f.Model.State.OpenEvent is null && f.Model.State.Occurrences.Count == 0
                && f.Model.State.Invitations.Count == 0 && f.Model.State.FilteredInvitations.Count == 0);
        });
        yield return ("Calendar cached ownership is rechecked before a selected event is shown", async () =>
        {
            using var f = new Fixture(); await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
            f.Source.Owned = [new(B)]; var reads = f.Source.DetailReads;
            Require(await f.Model.OpenEventAsync(action, Key("a")) is null && f.Source.DetailReads == reads && !f.Model.State.Enabled && f.Model.State.Occurrences.Count == 0);
        });
        yield return ("An old calendar refresh failure cannot erase a successful newer refresh", async () =>
        {
            using var f = new Fixture(); await f.Model.ActivateAsync(A, Features); using var first = f.Model.CaptureView()!.BeginAction()!; using var second = f.Model.CaptureView()!.BeginAction()!;
            var waiting = Signal(); var release = Signal(); var blocked = false;
            f.Source.Folders = async mailbox => { if (!blocked) { blocked = true; waiting.TrySetResult(); await release.Task; throw new IOException("Synthetic old read failure."); } return [new("cal_" + Name(mailbox))]; };
            var old = f.Model.ReloadAsync(first); await waiting.Task.WaitAsync(TimeSpan.FromSeconds(3)); await f.Model.ReloadAsync(second); release.TrySetResult(); await old;
            Require(f.Model.State.Enabled && f.Model.State.Error is null && !f.Model.State.Loading && f.Model.State.Occurrences.Single().Title == "A event");
        });
        yield return ("Retiring a calendar display binding does not cancel an already admitted independent gesture", async () =>
        {
            using var f = new Fixture(); await f.Model.ActivateAsync(A, Features); var view = f.Model.CaptureView()!; using var action = view.BeginAction()!; view.Retire();
            Require(view.BeginAction() is null && await f.Model.OpenEventAsync(action, Key("a")) is not null);
        });
        yield return ("Calendar refresh rechecks open detail and erases a removed invitation even if an old occurrence remains", async () =>
        {
            using var f = new Fixture(); await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
            Require(await f.Model.OpenEventAsync(action, Key("a")) is not null);
            f.Source.Detail = (_, _) => throw new PostboxException(404, "not_found"); await f.Model.ReloadAsync(action);
            Require(f.Model.State.Enabled && f.Model.State.OpenEvent is null && f.Model.State.Detail is null && f.Model.State.Occurrences.Count == 0
                && f.Model.State.Invitations.Count == 0 && f.Model.State.FilteredInvitations.Count == 0);
        });
        yield return ("Ownership denied during calendar recovery discovery erases the previously loaded calendar", async () =>
        {
            using var f = new Fixture();
            var folder = Path.Combine(Path.GetTempPath(), "pp-calendar-store-pending-" + Guid.NewGuid().ToString("N")); Directory.CreateDirectory(folder);
            using var protection = new MessageAttemptStoreTests.Protector();
            using var model = new CalendarStore(f.Source, f.Source, new CalendarDeliveryCoordinator(f.Source, new FileCalendarAttemptStore(folder, protection)), new Clock());
            try
            {
                f.Source.OnMailboxRead = count => { if (count == 2) f.Source.Owned = [new(B)]; };
                await model.ActivateAsync(A, Features);
                Require(!model.State.Enabled && model.State.Mailbox is null && model.State.Occurrences.Count == 0 && !model.State.CanCreate);
            }
            finally { Directory.Delete(folder, recursive: true); }
        });
        yield return ("The calendar controller creates only after protected recovery is authorized and releases its activity", async () =>
        {
            using var f = new Fixture(protectedDelivery: true); await f.Model.ActivateAsync(A, Features); Require(f.Model.State.CanCreate);
            using var action = f.Model.CaptureView()!.BeginAction()!;
            var result = await f.Model.CreateAsync(action, CalendarAttemptKind.Event, Body("event_controller"));
            Require(result?.Receipt.Event?.Event.Id == "evt_created" && result.PendingCleanup == false && f.Source.Posts == 1
                && f.Source.Committed.Count == 1 && f.Model.State.Pending.Count == 0 && !f.Model.State.Busy && f.Model.State.CanCreate);
        });
        yield return ("An uncertain calendar form sends again only through explicit exact recovery", async () =>
        {
            using var f = new Fixture(protectedDelivery: true); await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
            f.Source.LoseNext = true; Require(await f.Model.CreateAsync(action, CalendarAttemptKind.Event, Body("event_uncertain")) is null);
            var saved = f.Model.State.Pending.Single(); Require(f.Source.Posts == 1 && !f.Model.State.Busy);
            Require(await f.Model.CreateAsync(action, CalendarAttemptKind.Event, Body("event_replacement")) is null && f.Source.Posts == 1 && f.Model.State.Pending.Single().Id == saved.Id);
            var recovered = await f.Model.RetryAsync(action, saved);
            Require(recovered is not null && f.Source.Posts == 2 && f.Source.Committed.Count == 1 && f.Source.ExactRetries && f.Model.State.Pending.Count == 0 && !f.Model.State.Busy);
        });
        yield return ("Calendar controller discard removes local recovery without undoing an already committed event", async () =>
        {
            using var f = new Fixture(protectedDelivery: true); await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
            f.Source.LoseNext = true; await f.Model.CreateAsync(action, CalendarAttemptKind.Event, Body("event_discard")); var saved = f.Model.State.Pending.Single();
            Require(await f.Model.DiscardAsync(action, saved) && f.Model.State.Pending.Count == 0 && f.Source.Posts == 1 && f.Source.Committed.Count == 1 && !f.Model.State.Busy);
            Require(await f.Model.RetryAsync(action, saved) is null && !await f.Model.DiscardAsync(action, saved));
        });
        yield return ("A retired calendar create caller cannot persist or POST after fresh mailbox authorization", async () =>
        {
            using var f = new Fixture(protectedDelivery: true); await f.Model.ActivateAsync(A, Features); var caller = f.Source.Root.Child(); CalendarActionTarget action;
            using (new AuthorizationContext(caller)) action = f.Model.CaptureView()!.BeginAction()!; using var captured = action;
            var waiting = Signal(); var release = Signal(); var first = true;
            f.Source.MailboxDelay = async () => { if (first) { first = false; waiting.TrySetResult(); await release.Task; } };
            var create = f.Model.CreateAsync(action, CalendarAttemptKind.Event, Body("event_retired_controller")); await waiting.Task.WaitAsync(TimeSpan.FromSeconds(3));
            caller.Retire(); release.TrySetResult(); Require(await create is null && f.Source.Root.IsActive && f.Source.Posts == 0 && !f.Model.State.Busy && f.Model.State.Pending.Count == 0);
        });
    }
    private static CalendarEventKey Key(string suffix) => new("evt_" + suffix, "occ_" + suffix);
    private static CalendarWritePayload Body(string key) => CalendarWritePayload.From(new { request_key = key, title = "Controller fixture", start = "2026-10-28T09:00", end = "2026-10-28T10:00", tzid = "Europe/Istanbul" });
    private static CalendarEventResponse Detail(CalendarEventKey key) => new(new(key.EventId, My: new(Role: "attendee")), new(OccurrenceId: key.OccurrenceId));
    private static string Name(string mailbox) => mailbox == A ? "a" : "b";
    private static TaskCompletionSource Signal() => new(TaskCreationOptions.RunContinuationsAsynchronously);
    private static void Require(bool value) { if (!value) throw new Exception("Calendar store assertion failed."); }
    private sealed class Clock : TimeProvider { public override DateTimeOffset GetUtcNow() => DateTimeOffset.Parse("2026-10-27T00:00:00Z"); }
    private sealed class Fixture : IDisposable
    {
        internal readonly Source Source = new(); internal readonly CalendarStore Model;
        private readonly string? folder;
        private readonly MessageAttemptStoreTests.Protector? protector;
        internal Fixture(bool protectedDelivery = false)
        {
            if (protectedDelivery)
            {
                folder = Path.Combine(Path.GetTempPath(), "pp-calendar-controller-" + Guid.NewGuid().ToString("N")); Directory.CreateDirectory(folder);
                protector = new(); Model = new(Source, Source, new CalendarDeliveryCoordinator(Source, new FileCalendarAttemptStore(folder, protector), new Clock()), new Clock());
            }
            else Model = new(Source, Source, clock: new Clock());
        }
        public void Dispose() { Model.Dispose(); Source.Root.Retire(); protector?.Dispose(); if (folder is not null) Directory.Delete(folder, recursive: true); }
    }
    private sealed class Source : ICalendarService, IProtectedCalendarService
    {
        internal AuthorizationLease Root = new(); public AuthorizationLease? AcceptedAuthorization => Root;
        public string? RecoveryOwnerKey => new string('a', 64); public string? RecoveryOrigin => "https://calendar-store-fixture.invalid";
        internal IReadOnlyList<Mailbox> Owned = [new(A), new(B)];
        internal int MailboxReads, CalendarReads, OccurrenceReads, DetailReads;
        internal Action<int>? OnMailboxRead;
        internal Func<Task>? MailboxDelay;
        internal int Posts; internal bool LoseNext, ExactRetries = true;
        internal readonly Dictionary<string, string> Committed = [];
        internal Func<string, Task<IReadOnlyList<CalendarFolder>>>? Folders;
        internal Func<string, CalendarEventKey, Task<CalendarEventResponse>>? Detail;
        public async Task<IReadOnlyList<Mailbox>> GetMailboxesAsync(CancellationToken ct) { ct.ThrowIfCancellationRequested(); MailboxReads++; OnMailboxRead?.Invoke(MailboxReads); if (MailboxDelay is not null) await MailboxDelay(); return Owned; }
        public Task<ServerFeatures> GetCapabilitiesAsync(CancellationToken ct) { ct.ThrowIfCancellationRequested(); return Task.FromResult(Features); }
        public Task<CalendarCreateReceipt> SendPreparedCalendarAsync(MessageRecoveryScope scope, PreparedCalendarAttempt attempt, CancellationToken ct)
        {
            scope.RequireCurrent(ct); Posts++; var body = System.Text.Json.JsonSerializer.Serialize(attempt.Payload.Fields());
            if (Committed.TryGetValue(attempt.Id, out var existing)) ExactRetries &= existing == body; else Committed.Add(attempt.Id, body);
            if (LoseNext) { LoseNext = false; throw new HttpRequestException("Synthetic committed controller response loss."); }
            return Task.FromResult(attempt.Kind == CalendarAttemptKind.Event ? new CalendarCreateReceipt(attempt.Kind, Event: new(new("evt_created"))) : new CalendarCreateReceipt(attempt.Kind, Calendar: new("cal_created")));
        }
        public Task<IReadOnlyList<CalendarFolder>> GetCalendarsAsync(string mailbox, CancellationToken ct)
        { ct.ThrowIfCancellationRequested(); CalendarReads++; return Folders?.Invoke(mailbox) ?? Task.FromResult<IReadOnlyList<CalendarFolder>>([new("cal_" + Name(mailbox))]); }
        public Task<CalendarSettings> GetCalendarSettingsAsync(string mailbox, CancellationToken ct)
        { ct.ThrowIfCancellationRequested(); CalendarReads++; return Task.FromResult(new CalendarSettings(Timezone: "Europe/Istanbul", WeekStart: 1)); }
        public Task<IReadOnlyList<CalendarOccurrence>> GetCalendarOccurrencesAsync(string mailbox, long from, long to, IReadOnlyList<string>? calendars, CancellationToken ct)
        {
            ct.ThrowIfCancellationRequested(); CalendarReads++; OccurrenceReads++;
            return Task.FromResult<IReadOnlyList<CalendarOccurrence>>([new("occ_" + Name(mailbox), "evt_" + Name(mailbox), CalendarId: "cal_" + Name(mailbox),
                AllDay: false, StartsAt: DateTimeOffset.Parse("2026-10-27T06:00:00Z").ToUnixTimeSeconds(), EndsAt: DateTimeOffset.Parse("2026-10-27T07:00:00Z").ToUnixTimeSeconds(),
                Title: mailbox == A ? "A event" : "B event")]);
        }
        public Task<IReadOnlyList<CalendarInvitation>> GetCalendarInvitationsAsync(string mailbox, bool filtered, CancellationToken ct)
        { ct.ThrowIfCancellationRequested(); CalendarReads++; return Task.FromResult<IReadOnlyList<CalendarInvitation>>([new("evt_" + Name(mailbox), Title: (filtered ? "Filtered " : "Pending ") + Name(mailbox).ToUpperInvariant())]); }
        public Task<CalendarEventResponse> GetCalendarEventAsync(string mailbox, CalendarEventKey key, CancellationToken ct)
        { ct.ThrowIfCancellationRequested(); CalendarReads++; DetailReads++; return Detail?.Invoke(mailbox, key) ?? Task.FromResult(CalendarStoreTests.Detail(key)); }
        public Task<InboxSnapshot> LoadAsync(string identity, CancellationToken ct) => throw new NotSupportedException();
        public Task<SendReceipt> SendAsync(string identity, string peer, string body, string? thread, CancellationToken ct) => throw new NotSupportedException();
        public Task<string> CreateThreadAsync(string identity, string peer, string title, CancellationToken ct) => throw new NotSupportedException();
        public Task SetArchivedAsync(string identity, string peer, bool archived, CancellationToken ct) => throw new NotSupportedException();
        public Task AcknowledgeAsync(string identity, string message, CancellationToken ct) => throw new NotSupportedException();
        public Task SetContactAsync(string identity, Contact contact, CancellationToken ct) => throw new NotSupportedException();
        public Task RemoveContactAsync(string identity, string peer, CancellationToken ct) => throw new NotSupportedException();
        public Task DeleteThreadAsync(string identity, string thread, CancellationToken ct) => throw new NotSupportedException();
    }
}
