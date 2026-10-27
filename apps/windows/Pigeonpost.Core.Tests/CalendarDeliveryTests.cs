using System.Text;
using Pigeonpost.Core;
using Pigeonpost.Core.Calendar;
using Pigeonpost.Core.Groups;

internal static class CalendarDeliveryTests
{
    private const string Own = "/k/aaaaaaaaaaaaaaaaaaaaaaaaaa";
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Calendar creation is protected before its first POST and confirmed cleanup removes only that attempt", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            var prepared = await f.Coordinator.PrepareAsync(scope, CalendarAttemptKind.Event, Body("event_1"));
            f.Source.BeforePost = () => Require(f.Files().Length == 1 && !Encoding.UTF8.GetString(File.ReadAllBytes(f.Files().Single())).Contains("Private calendar fixture", StringComparison.Ordinal));
            var result = await f.Coordinator.SendAsync(scope, prepared);
            Require(!result.PendingCleanup && result.Receipt.Event?.Event.Id == "evt_fixture" && f.Source.Posts == 1 && f.Source.Committed.Count == 1 && f.Files().Length == 0);
        });
        yield return ("A lost calendar response survives restart and explicit retry uses the exact original key and body", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            var first = await f.Coordinator.PrepareAsync(scope, CalendarAttemptKind.Event, Body("event_restart")); f.Source.LoseNext = true;
            await Error<HttpRequestException>(() => f.Coordinator.SendAsync(scope, first));
            Require(f.Source.Posts == 1 && f.Source.Committed.Count == 1 && (await f.Coordinator.GetPendingAsync(scope)).Count == 1);
            f.Source.Rotate(); using var fresh = await f.Scope();
            var restarted = new CalendarDeliveryCoordinator(f.Source, new FileCalendarAttemptStore(f.Folder, f.Protector), f.Clock);
            var restored = (await restarted.GetPendingAsync(fresh)).Single();
            Require(restored.Id == first.Id && restored.CreatedAt == first.CreatedAt && restored.Payload.Fields().GetProperty("title").GetString() == "Private calendar fixture");
            Require(!(await restarted.SendAsync(fresh, restored)).PendingCleanup && f.Source.Posts == 2 && f.Source.Committed.Count == 1 && f.Source.ExactRetries && f.Files().Length == 0);
        });
        yield return ("Calendar protection failure prevents POST and leaves no pending plaintext or partial file", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            var coordinator = new CalendarDeliveryCoordinator(f.Source, new FileCalendarAttemptStore(f.Folder, new FailingProtector()), f.Clock);
            await Error<CalendarAttemptUnavailableException>(() => coordinator.PrepareAsync(scope, CalendarAttemptKind.Event, Body("event_fail")));
            Require(f.Source.Posts == 0 && f.Files().Length == 0 && !Directory.EnumerateFiles(f.Folder, "*.tmp", SearchOption.AllDirectories).Any());
        });
        yield return ("Conflicting calendar creation cannot overwrite a pending event or extend its replay deadline", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            var first = await f.Coordinator.PrepareAsync(scope, CalendarAttemptKind.Event, Body("event_first")); var encrypted = File.ReadAllBytes(f.Files().Single());
            await Error<CalendarAttemptConflictException>(() => f.Coordinator.PrepareAsync(scope, CalendarAttemptKind.Event, Body("event_other")));
            f.Clock.Now++;
            await Error<CalendarAttemptConflictException>(() => f.Coordinator.PrepareAsync(scope, CalendarAttemptKind.Event, Body("event_first")));
            Require(encrypted.SequenceEqual(File.ReadAllBytes(f.Files().Single())) && (await f.Coordinator.GetPendingAsync(scope)).Single().CreatedAt == first.CreatedAt);
        });
        yield return ("Calendar and event pending creations remain separate and one discard cannot erase the other", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            var folder = await f.Coordinator.PrepareAsync(scope, CalendarAttemptKind.Calendar, Body("folder_1"));
            var item = await f.Coordinator.PrepareAsync(scope, CalendarAttemptKind.Event, Body("event_1"));
            Require((await f.Coordinator.GetPendingAsync(scope)).Count == 2 && await f.Coordinator.DiscardAsync(scope, folder));
            Require((await f.Coordinator.GetPendingAsync(scope)).Single().Id == item.Id && f.Source.Posts == 0);
            Require(await f.Coordinator.DiscardAsync(scope, item) && f.Files().Length == 0 && f.Source.Committed.Count == 0);
        });
        yield return ("An older calendar review cannot discard a newer pending intent of the same kind", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            var old = await f.Coordinator.PrepareAsync(scope, CalendarAttemptKind.Event, Body("event_old")); await f.Coordinator.DiscardAsync(scope, old);
            var current = await f.Coordinator.PrepareAsync(scope, CalendarAttemptKind.Event, Body("event_current"));
            await Error<CalendarAttemptConflictException>(() => f.Coordinator.DiscardAsync(scope, old));
            Require((await f.Coordinator.GetPendingAsync(scope)).Single().Id == current.Id && f.Source.Posts == 0);
        });
        yield return ("Corrupt calendar recovery fails closed and cannot be silently replaced", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            await f.Coordinator.PrepareAsync(scope, CalendarAttemptKind.Event, Body("event_corrupt")); var path = f.Files().Single();
            var corrupt = File.ReadAllBytes(path); corrupt[^1] ^= 1; File.WriteAllBytes(path, corrupt);
            await Error<CalendarAttemptUnavailableException>(() => f.Coordinator.GetPendingAsync(scope));
            await Error<CalendarAttemptUnavailableException>(() => f.Coordinator.PrepareAsync(scope, CalendarAttemptKind.Calendar, Body("folder_new")));
            Require(corrupt.SequenceEqual(File.ReadAllBytes(path)) && f.Source.Posts == 0);
        });
        yield return ("Another signed-in owner or postbox origin cannot discover or send a calendar intent", async () =>
        {
            using var f = new Fixture(); using var original = await f.Scope();
            var prepared = await f.Coordinator.PrepareAsync(original, CalendarAttemptKind.Event, Body("event_owner"));
            f.Source.Rotate(owner: 'b'); using var other = await f.Scope();
            Require((await f.Coordinator.GetPendingAsync(other)).Count == 0);
            await Error<AuthorizationRetiredException>(() => f.Coordinator.SendAsync(other, prepared));
            f.Source.Rotate(); f.Source.Origin = "https://other-calendar-fixture.invalid"; using var elsewhere = await f.Scope();
            Require((await f.Coordinator.GetPendingAsync(elsewhere)).Count == 0 && f.Source.Posts == 0 && f.Files().Length == 1);
        });
        yield return ("A copied calendar ciphertext still refuses another owner's matching Windows protector", async () =>
        {
            using var f = new Fixture(); using var original = await f.Scope();
            await f.Coordinator.PrepareAsync(original, CalendarAttemptKind.Event, Body("event_owner_a")); var originalPath = f.Files().Single();
            f.Source.Rotate(owner: 'b'); using var other = await f.Scope();
            await f.Coordinator.PrepareAsync(other, CalendarAttemptKind.Event, Body("event_owner_b")); var copiedPath = f.Files().Single(p => p != originalPath);
            File.Copy(originalPath, copiedPath, overwrite: true);
            await Error<CalendarAttemptUnavailableException>(() => f.Coordinator.GetPendingAsync(other)); Require(f.Source.Posts == 0);
        });
        yield return ("Calendar caller retirement during protection prevents persistence and zeroes the plaintext buffer", async () =>
        {
            using var f = new Fixture(); var caller = f.Source.Root.Child(); MessageRecoveryScope scope;
            using (new AuthorizationContext(caller)) scope = await f.Scope(); using var captured = scope;
            var protection = new BlockingProtector(f.Protector);
            var coordinator = new CalendarDeliveryCoordinator(f.Source, new FileCalendarAttemptStore(f.Folder, protection), f.Clock);
            var pending = coordinator.PrepareAsync(scope, CalendarAttemptKind.Event, Body("event_retired"));
            await protection.Waiting.Task.WaitAsync(TimeSpan.FromSeconds(3)); caller.Retire(); protection.Release.TrySetResult();
            await Error<AuthorizationRetiredException>(async () => await pending);
            Require(f.Source.Root.IsActive && f.Source.Posts == 0 && f.Files().Length == 0 && protection.Plain is not null && protection.Plain.All(b => b == 0));
        });
        yield return ("An unretired account replacement during calendar protection cannot save under either owner", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope(); var original = f.Source.Root;
            var protection = new BlockingProtector(f.Protector);
            var coordinator = new CalendarDeliveryCoordinator(f.Source, new FileCalendarAttemptStore(f.Folder, protection), f.Clock);
            var pending = coordinator.PrepareAsync(scope, CalendarAttemptKind.Event, Body("event_replaced"));
            await protection.Waiting.Task.WaitAsync(TimeSpan.FromSeconds(3)); f.Source.Rotate(retire: false, owner: 'b'); protection.Release.TrySetResult();
            await Error<AuthorizationRetiredException>(async () => await pending);
            Require(original.IsActive && f.Source.Posts == 0 && f.Files().Length == 0); original.Retire();
        });
        yield return ("Expired or clock-reversed calendar intents cannot POST and remain explicitly discardable", async () =>
        {
            foreach (var difference in new[] { -1L, 86401L })
            {
                using var f = new Fixture(); using var scope = await f.Scope(); var pending = await f.Coordinator.PrepareAsync(scope, CalendarAttemptKind.Event, Body("event_old"));
                f.Clock.Now += difference; await Error<CalendarAttemptExpiredException>(() => f.Coordinator.SendAsync(scope, pending));
                Require(f.Source.Posts == 0 && (await f.Coordinator.GetPendingAsync(scope)).Count == 1 && await f.Coordinator.DiscardAsync(scope, pending));
            }
        });
        yield return ("Calendar confirmation remains successful when local recovery cleanup fails", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope(); var pending = await f.Coordinator.PrepareAsync(scope, CalendarAttemptKind.Event, Body("event_cleanup"));
            var coordinator = new CalendarDeliveryCoordinator(f.Source, new CleanupFailure(f.Store), f.Clock);
            var result = await coordinator.SendAsync(scope, pending);
            Require(result.PendingCleanup && result.Receipt.Event?.Event.Id == "evt_fixture" && f.Source.Committed.Count == 1 && (await f.Coordinator.GetPendingAsync(scope)).Count == 1);
            Require(!(await f.Coordinator.SendAsync(scope, pending)).PendingCleanup && f.Source.Committed.Count == 1 && f.Source.Posts == 2);
        });
        yield return ("Cancelling after a confirmed calendar response keeps the receipt and its cleanup reminder", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope(); using var cancellation = new CancellationTokenSource();
            var pending = await f.Coordinator.PrepareAsync(scope, CalendarAttemptKind.Event, Body("event_cancelled_cleanup")); f.Source.AfterCommit = cancellation.Cancel;
            var result = await f.Coordinator.SendAsync(scope, pending, cancellation.Token);
            Require(result.PendingCleanup && result.Receipt.Event is not null && f.Files().Length == 1 && f.Source.Committed.Count == 1);
        });
        yield return ("Calendar feature-off, foreign-client scopes and unpersisted intents cannot send", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope(); f.Source.Enabled = false;
            await Error<PostboxException>(() => f.Coordinator.PrepareAsync(scope, CalendarAttemptKind.Event, Body("event_disabled")));
            Require(f.Files().Length == 0); f.Source.Enabled = true;
            var pending = await f.Coordinator.PrepareAsync(scope, CalendarAttemptKind.Event, Body("event_original"));
            var foreign = new Source(); var foreignCoordinator = new CalendarDeliveryCoordinator(foreign, f.Store, f.Clock);
            await Error<AuthorizationRetiredException>(() => foreignCoordinator.SendAsync(scope, pending)); foreign.Root.Retire();
            await f.Coordinator.DiscardAsync(scope, pending);
            await Error<CalendarAttemptConflictException>(() => f.Coordinator.SendAsync(scope, pending)); Require(f.Source.Posts == 0);
        });
    }
    private static CalendarWritePayload Body(string key) => CalendarWritePayload.From(new { request_key = key, title = "Private calendar fixture", start = "2026-10-27T09:00", end = "2026-10-27T10:00", tzid = "Europe/Istanbul" });
    private static async Task<T> Error<T>(Func<Task> work) where T : Exception
    { try { await work(); } catch (T error) { return error; } throw new Exception("Expected " + typeof(T).Name); }
    private static void Require(bool value) { if (!value) throw new Exception("Calendar recovery assertion failed."); }
    private sealed class Clock : TimeProvider { internal long Now = 1793059200; public override DateTimeOffset GetUtcNow() => DateTimeOffset.FromUnixTimeSeconds(Now); }
    private sealed class Fixture : IDisposable
    {
        internal readonly string Folder = Path.Combine(Path.GetTempPath(), "pp-calendar-custody-" + Guid.NewGuid().ToString("N"));
        internal readonly Source Source = new(); internal readonly Clock Clock = new();
        internal readonly MessageAttemptStoreTests.Protector Protector = new(); internal readonly FileCalendarAttemptStore Store;
        internal readonly CalendarDeliveryCoordinator Coordinator;
        internal Fixture() { Directory.CreateDirectory(Folder); Store = new(Folder, Protector); Coordinator = new(Source, Store, Clock); }
        internal async Task<MessageRecoveryScope> Scope() => await MessageRecoveryScope.AuthorizeAsync(Source, Own) ?? throw new Exception("Fixture authorization missing.");
        internal string[] Files() => Directory.GetFiles(Folder, "*.pending", SearchOption.AllDirectories);
        public void Dispose() { Source.Root.Retire(); Protector.Dispose(); Directory.Delete(Folder, recursive: true); }
    }
    private sealed class Source : IProtectedCalendarService
    {
        internal AuthorizationLease Root = new(); internal char Owner = 'a'; internal string Origin = "https://calendar-custody-fixture.invalid";
        public AuthorizationLease? AcceptedAuthorization => Root; public string? RecoveryOwnerKey => new string(Owner, 64); public string? RecoveryOrigin => Origin;
        internal int Posts; internal bool LoseNext, Enabled = true, ExactRetries = true; internal Action? BeforePost, AfterCommit;
        internal readonly Dictionary<string, string> Committed = [];
        internal void Rotate(bool retire = true, char owner = 'a') { if (retire) Root.Retire(); Root = new(); Owner = owner; }
        public Task<ServerFeatures> GetCapabilitiesAsync(CancellationToken ct) { ct.ThrowIfCancellationRequested(); return Task.FromResult(new ServerFeatures(new HashSet<string>(Enabled ? [ServerFeatures.Calendar] : []))); }
        public Task<IReadOnlyList<Mailbox>> GetMailboxesAsync(CancellationToken ct) { ct.ThrowIfCancellationRequested(); return Task.FromResult<IReadOnlyList<Mailbox>>([new(Own)]); }
        public Task<CalendarCreateReceipt> SendPreparedCalendarAsync(MessageRecoveryScope scope, PreparedCalendarAttempt attempt, CancellationToken ct)
        {
            scope.RequireCurrent(ct); BeforePost?.Invoke(); Posts++;
            var key = attempt.Kind + ":" + attempt.Id; var encoded = System.Text.Json.JsonSerializer.Serialize(attempt.Payload.Fields());
            if (Committed.TryGetValue(key, out var original)) ExactRetries &= original == encoded; else Committed.Add(key, encoded);
            if (LoseNext) { LoseNext = false; throw new HttpRequestException("Synthetic committed calendar response loss."); }
            var receipt = attempt.Kind == CalendarAttemptKind.Calendar ? new CalendarCreateReceipt(attempt.Kind, Calendar: new("cal_fixture"))
                : new CalendarCreateReceipt(attempt.Kind, Event: new(new("evt_fixture")));
            AfterCommit?.Invoke(); return Task.FromResult(receipt);
        }
        public Task<InboxSnapshot> LoadAsync(string identity, CancellationToken ct) => throw new NotSupportedException();
        public Task<SendReceipt> SendAsync(string identity, string peer, string body, string? thread, CancellationToken ct) => throw new NotSupportedException();
        public Task<string> CreateThreadAsync(string identity, string peer, string title, CancellationToken ct) => throw new NotSupportedException();
        public Task SetArchivedAsync(string identity, string peer, bool archived, CancellationToken ct) => throw new NotSupportedException();
        public Task AcknowledgeAsync(string identity, string message, CancellationToken ct) => throw new NotSupportedException();
        public Task SetContactAsync(string identity, Contact contact, CancellationToken ct) => throw new NotSupportedException();
        public Task RemoveContactAsync(string identity, string peer, CancellationToken ct) => throw new NotSupportedException();
        public Task DeleteThreadAsync(string identity, string thread, CancellationToken ct) => throw new NotSupportedException();
    }
    private sealed class FailingProtector : IStateProtector
    {
        public Task<byte[]> ProtectAsync(byte[] plain) => throw new IOException("Synthetic protection failure.");
        public Task<byte[]> UnprotectAsync(byte[] data) => throw new NotSupportedException();
    }
    private sealed class BlockingProtector(IStateProtector inner) : IStateProtector
    {
        internal readonly TaskCompletionSource Waiting = new(TaskCreationOptions.RunContinuationsAsynchronously), Release = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal byte[]? Plain;
        public async Task<byte[]> ProtectAsync(byte[] plain) { Plain = plain; var value = await inner.ProtectAsync(plain); Waiting.TrySetResult(); await Release.Task; return value; }
        public Task<byte[]> UnprotectAsync(byte[] data) => inner.UnprotectAsync(data);
    }
    private sealed class CleanupFailure(ICalendarAttemptStore inner) : ICalendarAttemptStore
    {
        public bool Persistent => true;
        public Task<IReadOnlyList<PreparedCalendarAttempt>> ListAsync(MessageRecoveryScope scope, CancellationToken ct = default) => inner.ListAsync(scope, ct);
        public Task SaveAsync(MessageRecoveryScope scope, PreparedCalendarAttempt attempt, CancellationToken ct = default) => inner.SaveAsync(scope, attempt, ct);
        public Task<bool> RemoveAsync(MessageRecoveryScope scope, PreparedCalendarAttempt expected, CancellationToken ct = default) => throw new IOException("Synthetic cleanup failure.");
    }
}
