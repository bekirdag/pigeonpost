using Pigeonpost.Core;

internal static class MessageDeliveryCoordinatorTests
{
    private const string Own = "/k/aaaaaaaaaaaaaaaaaaaaaaaaaa", Peer = "/k/bbbbbbbbbbbbbbbbbbbbbbbbbb", Other = "/k/cccccccccccccccccccccccccc";
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Preparing a delivery freezes its canonical route and original reference before any send", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope(); var files = new[] { "attachment_fixture" };
            var request = new MessageSendRequest(Own, "/reviewed/main", "Prepared fixture", attachments: files); files[0] = "changed_fixture";
            var review = new MessageDeliveryReview(request.To, "Reviewed subject", new string('d', 32));
            var prepared = await f.Coordinator.PrepareAsync(scope, request, review);
            Require(prepared.Request.To == Peer && prepared.Request.MsgRef == request.MsgRef && prepared.Request.Attachments!.Single() == "attachment_fixture"
                && prepared.Review == review && f.Service.Deliveries.Count == 0 && (await f.Store.ListAsync(scope)).Single().Request.MsgRef == request.MsgRef);
        });
        yield return ("A protected persistence failure prevents the first delivery POST", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope(); f.Store.FailSave = true;
            await Fails<IOException>(() => f.Coordinator.PrepareAsync(scope, new(Own, "/reviewed/main", "Unpersisted fixture")));
            Require(f.Service.Deliveries.Count == 0 && (await f.Store.ListAsync(scope)).Count == 0);
        });
        yield return ("Restart recovery never re-resolves an alias and explicit retry uses the original committed request", async () =>
        {
            using var f = new Fixture(); using var original = await f.Scope();
            var prepared = await f.Coordinator.PrepareAsync(original, new(Own, "/reviewed/main", "Committed loss fixture"), new("/reviewed/main", "Original subject"));
            f.Service.LoseNext = true; await Fails<HttpRequestException>(() => f.Coordinator.SendAsync(original, prepared));
            Require(f.Service.Deliveries.Count == 1 && f.Service.Committed.Count == 1);
            f.Service.Rotate(); f.Service.Peer = Other; using var current = await f.Scope();
            var restarted = new MessageDeliveryCoordinator(f.Service, new FileMessageAttemptStore(f.Folder, f.Protector));
            var pending = (await restarted.GetPendingAsync(current)).Single(); var lookups = f.Service.Lookups;
            Require(f.Service.Deliveries.Count == 1 && pending.Request.To == Peer && pending.Review.Recipient == "/reviewed/main");
            var result = await restarted.SendAsync(current, pending);
            Require(result.Receipt.Duplicate == true && !result.PendingCleanup && f.Service.Lookups == lookups && f.Service.Committed.Count == 1
                && f.Service.Deliveries.Count == 2 && f.Service.Deliveries.All(p => p.MsgRef == prepared.Request.MsgRef && p.To == Peer && p.Body == prepared.Request.Body)
                && (await restarted.GetPendingAsync(current)).Count == 0);
        });
        yield return ("An outstanding reviewed alias is found before a new handle lookup can conceal it", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            var first = await f.Coordinator.PrepareAsync(scope, new(Own, "/reviewed/main", "Original intent")); var lookups = f.Service.Lookups;
            f.Service.Peer = Other;
            await Fails<MessageAttemptConflictException>(() => f.Coordinator.PrepareAsync(scope, new(Own, "/reviewed/main", "Replacement intent")));
            Require(f.Service.Lookups == lookups && f.Service.Deliveries.Count == 0 && (await f.Store.ListAsync(scope)).Single().Request.MsgRef == first.Request.MsgRef);
        });
        yield return ("Confirmed delivery remains confirmed when protected cleanup fails", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            var prepared = await f.Coordinator.PrepareAsync(scope, new(Own, "/reviewed/main", "Confirmed fixture")); f.Store.FailRemove = true;
            var result = await f.Coordinator.SendAsync(scope, prepared);
            Require(result.PendingCleanup && result.Receipt.MsgRef == prepared.Request.MsgRef && f.Service.Committed.Count == 1 && (await f.Store.ListAsync(scope)).Count == 1);
            f.Store.FailRemove = false; Require(await f.Coordinator.DiscardAsync(scope, prepared) && f.Service.Deliveries.Count == 1);
        });
        yield return ("Delivery refuses an unpersisted or substituted request before POST", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            var naked = PreparedMessageAttempt.Create(scope, new(Own, Peer, "Not saved fixture"));
            await Fails<MessageAttemptUnavailableException>(() => f.Coordinator.SendAsync(scope, naked));
            var saved = await f.Coordinator.PrepareAsync(scope, new(Own, "/reviewed/main", "Original saved fixture"));
            var altered = PreparedMessageAttempt.Create(scope, new(Own, Peer, "Substituted fixture", msgRef: saved.Request.MsgRef), saved.Review);
            await Fails<MessageAttemptConflictException>(() => f.Coordinator.SendAsync(scope, altered));
            Require(f.Service.Deliveries.Count == 0);
        });
        yield return ("A retired delivery actor cannot send or erase its persisted request", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            var prepared = await f.Coordinator.PrepareAsync(scope, new(Own, "/reviewed/main", "Retired fixture")); f.Service.Rotate();
            await Fails<AuthorizationRetiredException>(() => f.Coordinator.SendAsync(scope, prepared));
            await Fails<AuthorizationRetiredException>(() => f.Coordinator.DiscardAsync(scope, prepared));
            using var current = await f.Scope(); Require((await f.Store.ListAsync(current)).Count == 1 && f.Service.Deliveries.Count == 0);
        });
        yield return ("Caller retirement during lookup prevents persistence and delivery", async () =>
        {
            using var f = new Fixture(); var caller = f.Service.Root.Child(); MessageRecoveryScope scope;
            using (new AuthorizationContext(caller)) scope = await f.Scope();
            using (scope)
            {
                f.Service.BlockLookup = true;
                var prepare = f.Coordinator.PrepareAsync(scope, new(Own, "/reviewed/main", "Delayed fixture"));
                await f.Service.Waiting.Task.WaitAsync(TimeSpan.FromSeconds(3)); caller.Retire(); f.Service.Release.TrySetResult();
                await Fails<AuthorizationRetiredException>(() => prepare);
                Require(f.Service.Deliveries.Count == 0 && !Directory.EnumerateFiles(f.Folder, "*.pending", SearchOption.AllDirectories).Any() && f.Service.Root.IsActive);
            }
        });
        yield return ("Discarding an older reviewed request cannot remove a newer pending delivery", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            var original = await f.Coordinator.PrepareAsync(scope, new(Own, "/reviewed/main", "Original fixture"));
            Require(await f.Coordinator.DiscardAsync(scope, original));
            var newer = await f.Coordinator.PrepareAsync(scope, new(Own, "/reviewed/main", "Newer fixture"));
            await Fails<MessageAttemptConflictException>(() => f.Coordinator.DiscardAsync(scope, original));
            Require((await f.Store.ListAsync(scope)).Single().Request.MsgRef == newer.Request.MsgRef && f.Service.Deliveries.Count == 0);
        });
        yield return ("A coordinator cannot use another service's authorized recovery scope", async () =>
        {
            using var f = new Fixture(); using var other = new Fixture(); using var scope = await f.Scope();
            await Fails<AuthorizationRetiredException>(() => other.Coordinator.GetPendingAsync(scope));
            await Fails<AuthorizationRetiredException>(() => other.Coordinator.PrepareAsync(scope, new(Own, "/reviewed/main", "Other service fixture")));
            Require(other.Service.Lookups == 0 && other.Service.Deliveries.Count == 0);
        });
        yield return ("An account retired after server confirmation cannot publish a result or erase recovery", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            var prepared = await f.Coordinator.PrepareAsync(scope, new(Own, "/reviewed/main", "Retired confirmation fixture")); f.Service.AfterCommit = f.Service.Rotate;
            await Fails<AuthorizationRetiredException>(() => f.Coordinator.SendAsync(scope, prepared));
            using var current = await f.Scope(); Require(f.Service.Committed.Count == 1 && (await f.Store.ListAsync(current)).Count == 1);
        });
        yield return ("Cancellation after a confirmed response keeps the receipt and a protected cleanup reminder", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope(); using var cancellation = new CancellationTokenSource();
            var prepared = await f.Coordinator.PrepareAsync(scope, new(Own, "/reviewed/main", "Confirmed cancellation fixture")); f.Service.AfterCommit = cancellation.Cancel;
            var result = await f.Coordinator.SendAsync(scope, prepared, cancellation.Token);
            Require(result.PendingCleanup && result.Receipt.MsgRef == prepared.Request.MsgRef && (await f.Store.ListAsync(scope)).Count == 1 && f.Service.Committed.Count == 1);
        });
    }
    private static void Require(bool value) { if (!value) throw new Exception("Delivery coordinator assertion failed."); }
    private static async Task Fails<T>(Func<Task> action) where T : Exception
    { try { await action(); } catch (T) { return; } throw new Exception("Delivery coordinator unexpectedly accepted."); }
    private sealed class Fixture : IDisposable
    {
        internal readonly string Folder = Path.Combine(Path.GetTempPath(), "pp-coordinator-fixture-" + Guid.NewGuid().ToString("N"));
        internal readonly MessageAttemptStoreTests.Protector Protector = new();
        internal readonly Service Service = new(); internal readonly Store Store; internal readonly MessageDeliveryCoordinator Coordinator;
        internal Fixture() { Directory.CreateDirectory(Folder); Store = new(new FileMessageAttemptStore(Folder, Protector)); Coordinator = new(Service, Store); }
        internal async Task<MessageRecoveryScope> Scope() => await MessageRecoveryScope.AuthorizeAsync(Service, Own) ?? throw new Exception("Fixture scope missing.");
        public void Dispose() { Service.Release.TrySetResult(); Service.Root.Retire(); Protector.Dispose(); Directory.Delete(Folder, recursive: true); }
    }
    private sealed class Store(IMessageAttemptStore inner) : IMessageAttemptStore
    {
        internal bool FailSave, FailRemove;
        public bool Persistent => true;
        public Task<IReadOnlyList<PreparedMessageAttempt>> ListAsync(MessageRecoveryScope s, CancellationToken ct = default) => inner.ListAsync(s, ct);
        public Task<PreparedMessageAttempt?> LoadAsync(MessageRecoveryScope s, string to, string? thread, CancellationToken ct = default) => inner.LoadAsync(s, to, thread, ct);
        public Task SaveAsync(MessageRecoveryScope s, PreparedMessageAttempt a, CancellationToken ct = default) => FailSave ? throw new IOException("Synthetic disk refusal.") : inner.SaveAsync(s, a, ct);
        public Task<bool> RemoveAsync(MessageRecoveryScope s, string to, string? thread, string reference, CancellationToken ct = default) => FailRemove ? throw new IOException("Synthetic cleanup refusal.") : inner.RemoveAsync(s, to, thread, reference, ct);
    }
    private sealed class Service : IProtectedMessageService
    {
        internal AuthorizationLease Root = new();
        public AuthorizationLease? AcceptedAuthorization => Root;
        public string? RecoveryOwnerKey => new string('a', 64);
        public string? RecoveryOrigin => "http://127.0.0.1:9185";
        internal string Peer = MessageDeliveryCoordinatorTests.Peer;
        internal int Lookups; internal bool LoseNext, BlockLookup; internal Action? AfterCommit;
        internal readonly List<MessageSendRequest> Deliveries = []; internal readonly HashSet<string> Committed = [];
        internal readonly TaskCompletionSource Waiting = new(TaskCreationOptions.RunContinuationsAsynchronously), Release = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal void Rotate() { Root.Retire(); Root = new(); }
        public async Task<RecipientRoute> ResolveRecipientAsync(MessageRecoveryScope scope, string reviewedRecipient, CancellationToken ct)
        {
            scope.RequireCurrent(ct); Lookups++;
            if (BlockLookup) { Waiting.TrySetResult(); await Release.Task.ConfigureAwait(false); }
            scope.RequireCurrent(ct); return new(Own, Peer, reviewedRecipient);
        }
        public Task<SendReceipt> SendPreparedMessageAsync(MessageRecoveryScope scope, PreparedMessageAttempt attempt, CancellationToken ct)
        {
            scope.RequireCurrent(ct); Deliveries.Add(attempt.Request); var duplicate = !Committed.Add(attempt.Request.MsgRef); AfterCommit?.Invoke();
            if (LoseNext) { LoseNext = false; throw new HttpRequestException("Synthetic committed response loss."); }
            return Task.FromResult(new SendReceipt("recipient_copy", "sent_copy", attempt.Request.MsgRef, duplicate));
        }
        public Task<IReadOnlyList<Mailbox>> GetMailboxesAsync(CancellationToken ct) => Task.FromResult<IReadOnlyList<Mailbox>>([new(Own)]);
        public Task<InboxSnapshot> LoadAsync(string identity, CancellationToken ct) => throw new NotSupportedException();
        public Task<SendReceipt> SendAsync(string identity, string peer, string body, string? threadId, CancellationToken ct) => throw new NotSupportedException();
        public Task<string> CreateThreadAsync(string identity, string peer, string title, CancellationToken ct) => throw new NotSupportedException();
        public Task SetArchivedAsync(string identity, string peer, bool archived, CancellationToken ct) => throw new NotSupportedException();
        public Task AcknowledgeAsync(string identity, string messageId, CancellationToken ct) => throw new NotSupportedException();
        public Task SetContactAsync(string identity, Contact contact, CancellationToken ct) => throw new NotSupportedException();
        public Task RemoveContactAsync(string identity, string peer, CancellationToken ct) => throw new NotSupportedException();
        public Task DeleteThreadAsync(string identity, string threadId, CancellationToken ct) => throw new NotSupportedException();
    }
}
