using Pigeonpost.Core;
using Pigeonpost.Core.Calendar;

internal static partial class CalendarStoreTests
{
    public static IEnumerable<(string Name, Func<Task> Run)> Meetings()
    {
        yield return ("Scheduled meeting codes normalize only the ten-character locator alphabet and optional display hyphen", () =>
        {
            Require(CalendarMeetingCode.Parse("abcde-fghjk") == "ABCDEFGHJK" && CalendarMeetingCode.Parse("ABCDEFGHJK") == "ABCDEFGHJK");
            foreach (var invalid in new[] { "AbcdeFGHj", "ABCDEFGHj-", "ABCDEFG-Hjk", "ABCDEFGHJ K", "ABCDEFGH J", "ABCDEFGH0K", "https://fixture.invalid/m/ABCDEFGHJK", " ABCDEFGHJK", "ABCDEF" })
                Require(CalendarMeetingCode.Parse(invalid) is null);
            return Task.CompletedTask;
        });
        yield return ("A scheduled join retains fresh canonical membership, exact occurrence, media and a bounded Unicode title", async () =>
        {
            using var f = new Fixture(); var title = string.Concat(Enumerable.Repeat("😀", 201));
            f.Source.Detail = (_, key) => Task.FromResult(Meeting(key) with { Occurrence = new(OccurrenceId: key.OccurrenceId, Title: title) });
            await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!; var reads = f.Source.MailboxReads;
            using var join = await f.Model.PrepareScheduledJoinAsync(action, Key("a"), "abcde-fghjk", true, f.Model.State.ContextId);
            Require(join is not null && join.Mailbox == A && join.OccurrenceId == Key("a").OccurrenceId && join.EventId == "evt_a" && join.Code == "ABCDEFGHJK"
                && join.Video && join.Title.EnumerateRunes().Count() == 200 && f.Source.MailboxReads > reads && !join.ToString().Contains("ABCDEFGHJK", StringComparison.Ordinal));
            join!.RequireCurrent(f.Source.Root, A);
        });
        yield return ("A stale context, missing occurrence or ad-hoc code cannot start scheduled admission", async () =>
        {
            using var f = new Fixture(); f.Source.Detail = (_, key) => Task.FromResult(Meeting(key)); await f.Model.ActivateAsync(A, Features);
            using var action = f.Model.CaptureView()!.BeginAction()!; var reads = f.Source.DetailReads;
            Require(await f.Model.PrepareScheduledJoinAsync(action, Key("a"), "ABCDEFGHJK", false, Guid.NewGuid()) is null);
            Require(await f.Model.PrepareScheduledJoinAsync(action, new("evt_a"), "ABCDEFGHJK", false, f.Model.State.ContextId) is null);
            Require(await f.Model.PrepareScheduledJoinAsync(action, Key("a"), "ABCDEF", false, f.Model.State.ContextId) is null && f.Source.DetailReads == reads);
        });
        yield return ("Scheduled admission refuses cancelled, all-day, declined, unknown-role, disabled and replaced meetings", async () =>
        {
            var good = Meeting(Key("a"));
            var invalid = new[] { good with { Event = good.Event with { Status = "cancelled" } }, good with { Occurrence = new(OccurrenceId: "occ_a", Status: "cancelled") },
                good with { Event = good.Event with { AllDay = true } }, good with { Event = good.Event with { My = new(Role: "attendee", AttendeeId: "att_a", Rsvp: "declined") } },
                good with { Occurrence = new(OccurrenceId: "occ_a", MyRsvp: "declined") }, good with { Event = good.Event with { My = new(Role: "future_host") } },
                good with { Event = good.Event with { My = new(Role: "attendee") } }, good with { Event = good.Event with { Meeting = new(Enabled: false, Code: "ABCDEFGHJK") } },
                good with { Event = good.Event with { Meeting = new(Code: "BCDEFGHJKM") } } };
            foreach (var detail in invalid)
            {
                using var f = new Fixture(); f.Source.Detail = (_, _) => Task.FromResult(detail); await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
                Require(await f.Model.PrepareScheduledJoinAsync(action, Key("a"), "ABCDEFGHJK", false, f.Model.State.ContextId) is null && f.Source.Updates == 0 && f.Source.Posts == 0);
            }
        });
        yield return ("Losing mailbox ownership prevents meeting detail and cannot prepare a scheduled join", async () =>
        {
            using var f = new Fixture(); f.Source.Detail = (_, key) => Task.FromResult(Meeting(key)); await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
            f.Source.Owned = [new(B)]; var reads = f.Source.DetailReads;
            Require(await f.Model.PrepareScheduledJoinAsync(action, Key("a"), "ABCDEFGHJK", false, f.Model.State.ContextId) is null && f.Source.DetailReads == reads && !f.Model.State.Enabled);
        });
        yield return ("A substituted scheduled event or occurrence response grants no join intent", async () =>
        {
            foreach (var key in new[] { new CalendarEventKey("evt_other", "occ_a"), new CalendarEventKey("evt_a", "occ_other") })
            {
                using var f = new Fixture(); f.Source.Detail = (_, _) => Task.FromResult(Meeting(key)); await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
                Require(await f.Model.PrepareScheduledJoinAsync(action, Key("a"), "ABCDEFGHJK", false, f.Model.State.ContextId) is null);
            }
        });
        yield return ("A retired original meeting caller cannot prepare a join after a delayed detail response", async () =>
        {
            using var f = new Fixture(); await f.Model.ActivateAsync(A, Features); var caller = f.Source.Root.Child(); CalendarActionTarget action;
            using (new AuthorizationContext(caller)) action = f.Model.CaptureView()!.BeginAction()!; using var captured = action;
            var waiting = Signal(); var release = Signal(); f.Source.Detail = async (_, key) => { waiting.TrySetResult(); await release.Task; return Meeting(key); };
            var prepare = f.Model.PrepareScheduledJoinAsync(action, Key("a"), "ABCDEFGHJK", false, f.Model.State.ContextId);
            await waiting.Task.WaitAsync(TimeSpan.FromSeconds(3)); caller.Retire(); release.TrySetResult(); Require(await prepare is null && f.Source.Root.IsActive);
        });
        yield return ("A scheduled service binding retains a later independent invocation caller across host awaits", async () =>
        {
            using var f = new Fixture(); f.Source.Detail = (_, key) => Task.FromResult(Meeting(key)); await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
            using var join = (await f.Model.PrepareScheduledJoinAsync(action, Key("a"), "ABCDEFGHJK", false, f.Model.State.ContextId))!;
            var caller = f.Source.Root.Child(); CalendarJoinBinding bound; using (new AuthorizationContext(caller)) bound = join.BindForService(new MeetingTokens(f.Source), A); using var binding = bound;
            bound.RequireCurrent(); caller.Retire(); Require(Retired(bound.RequireCurrent)); join.RequireCurrent(f.Source.Root, A);
        });
        yield return ("A scheduled intent cannot bind a different account, inbox or replacement provider", async () =>
        {
            using var f = new Fixture(); f.Source.Detail = (_, key) => Task.FromResult(Meeting(key)); await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
            using var join = (await f.Model.PrepareScheduledJoinAsync(action, Key("a"), "ABCDEFGHJK", false, f.Model.State.ContextId))!;
            Require(Retired(() => join.BindForService(new MeetingTokens(f.Source), B)));
            var original = f.Source.Root; f.Source.Root = new(); Require(original.IsActive && Retired(() => join.RequireCurrent(original, A)) && Retired(() => join.BindForService(new MeetingTokens(f.Source), A))); original.Retire();
        });
        yield return ("Leaving the calendar inbox retires a prepared join even when its account is still accepted", async () =>
        {
            using var f = new Fixture(); f.Source.Detail = (_, key) => Task.FromResult(Meeting(key)); await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
            using var join = (await f.Model.PrepareScheduledJoinAsync(action, Key("a"), "ABCDEFGHJK", false, f.Model.State.ContextId))!;
            await f.Model.ActivateAsync(B, Features); Require(f.Source.Root.IsActive && Retired(() => join.RequireCurrent(f.Source.Root, A)));
        });
        yield return ("An explicitly disposed meeting intent and its old gesture cannot be revived for another call", async () =>
        {
            using var f = new Fixture(); f.Source.Detail = (_, key) => Task.FromResult(Meeting(key)); await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
            using var join = (await f.Model.PrepareScheduledJoinAsync(action, Key("a"), "ABCDEFGHJK", false, f.Model.State.ContextId))!;
            using var binding = join.BindForService(new MeetingTokens(f.Source), A); join.Dispose(); Require(Retired(binding.RequireCurrent) && Retired(() => join.RequireCurrent(f.Source.Root, A)));
        });
    }
    private static CalendarEventResponse Meeting(CalendarEventKey key) => Editable(key) with { Event = Editable(key).Event with { Meeting = new(Code: "ABCDEFGHJK") } };
    private static bool Retired(Action work) { try { work(); return false; } catch (AuthorizationRetiredException) { return true; } }
    private sealed class MeetingTokens(Source source) : IAccessTokenProvider
    {
        public AuthorizationLease? AcceptedAuthorization => source.Root;
        public Task<string> GetTokenAsync(CancellationToken ct) => throw new InvalidOperationException("The typed join never requests a token.");
        public Task<string> RefreshTokenAsync(CancellationToken ct) => throw new InvalidOperationException("The typed join never requests a token.");
    }
}
