using Pigeonpost.Core;

internal static class MessageRecoveryScopeTests
{
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("A recovery scope freshly authorizes its exact canonical inbox and disposes only its local actor", async () =>
        {
            using var f = new Service(); var scope = await MessageRecoveryScope.AuthorizeAsync(f, Own);
            Require(scope is { IsCurrent: true } && scope.OwnerKey == f.Key && scope.Mailbox == Own && f.Lists == 1);
            Require(scope!.ToString() == "MessageRecoveryScope(redacted)"); scope.Dispose();
            Require(!scope.IsCurrent && f.Root!.IsActive && f.ObservedActor?.IsActive == false);
        });
        yield return ("An owner hint without an accepted grant or with an unknown partition grants no recovery access", async () =>
        {
            using var f = new Service(); f.Root!.Retire(); f.Root = null;
            Require(await MessageRecoveryScope.AuthorizeAsync(f, Own) is null && f.Lists == 0);
            f.Root = new(); f.Key = null;
            Require(await MessageRecoveryScope.AuthorizeAsync(f, Own) is null && f.Lists == 0);
            f.Key = new string('A', 64);
            Require(await MessageRecoveryScope.AuthorizeAsync(f, Own) is null && f.Lists == 0);
        });
        yield return ("A canonical-looking handle cannot substitute for current server mailbox ownership", async () =>
        {
            using var f = new Service(); f.Boxes = [new(Other, Own)];
            await Fails<MessageRecoveryNotAuthorizedException>(() => MessageRecoveryScope.AuthorizeAsync(f, Own));
            Require(f.Lists == 1 && f.ObservedActor?.IsActive == false);
        });
        yield return ("A recovery authorization cannot adopt an unretired replacement provider grant", async () =>
        {
            using var f = new Service(); var previous = f.Root!;
            f.OnList = () => f.Root = new();
            try { await Fails<AuthorizationRetiredException>(() => MessageRecoveryScope.AuthorizeAsync(f, Own)); }
            finally { previous.Retire(); }
            Require(f.ObservedActor?.IsActive == false);
        });
        yield return ("Recovery refuses an owner partition change even when the provider kept its original grant", async () =>
        {
            using var f = new Service(); f.OnList = () => f.Key = new string('b', 64);
            await Fails<AuthorizationRetiredException>(() => MessageRecoveryScope.AuthorizeAsync(f, Own));
            Require(f.Root!.IsActive && f.ObservedActor?.IsActive == false);
        });
        yield return ("A recovery scope retains every original caller constraint after nested scopes return", async () =>
        {
            using var f = new Service(); var outer = f.Root!.Child(); var inner = f.Root.Child();
            using var first = new AuthorizationContext(outer); using var second = new AuthorizationContext(inner);
            using var scope = await MessageRecoveryScope.AuthorizeAsync(f, Own);
            Require(scope!.IsCurrent && ReferenceEquals(AuthorizationContext.Current, inner));
            outer.Retire(); Require(!scope.IsCurrent && inner.IsActive && f.Root.IsActive); inner.Retire();
        });
        yield return ("A retired caller cannot authorize recovery for a newly accepted account", async () =>
        {
            using var f = new Service(); var previous = f.Root!; using var caller = new AuthorizationContext(previous.Child());
            previous.Retire(); f.Root = new();
            await Fails<AuthorizationRetiredException>(() => MessageRecoveryScope.AuthorizeAsync(f, Own));
            Require(f.Lists == 0 && f.KeyReads == 0);
        });
        yield return ("A foreign ambient caller cannot use an otherwise active recovery scope", async () =>
        {
            using var f = new Service(); using var scope = await MessageRecoveryScope.AuthorizeAsync(f, Own);
            var foreign = new AuthorizationLease();
            using (new AuthorizationContext(foreign)) Require(!scope!.IsCurrent);
            Require(scope!.IsCurrent); foreign.Retire();
        });
        yield return ("Cancelled or no-longer-owned recovery authorization never returns a live scope", async () =>
        {
            using var f = new Service(); using var cancel = new CancellationTokenSource(); f.OnList = cancel.Cancel;
            await Fails<OperationCanceledException>(() => MessageRecoveryScope.AuthorizeAsync(f, Own, cancel.Token));
            Require(f.ObservedActor?.IsActive == false);
        });
        yield return ("Recovery refuses aliases and malformed canonical inboxes before any service request", async () =>
        {
            using var f = new Service();
            foreach (var identity in new[] { "/alice/main", "/k/short", "/k/" + new string('A', 26), "/k/" + new string('a', 25) + "/" })
                await Fails<ArgumentException>(() => MessageRecoveryScope.AuthorizeAsync(f, identity));
            Require(f.Lists == 0 && f.KeyReads == 0);
        });
    }
    private const string Own = "/k/aaaaaaaaaaaaaaaaaaaaaaaaaa", Other = "/k/bbbbbbbbbbbbbbbbbbbbbbbbbb";
    private static void Require(bool value) { if (!value) throw new Exception("Recovery authorization assertion failed."); }
    private static async Task Fails<T>(Func<Task> action) where T : Exception
    { try { await action(); } catch (T) { return; } throw new Exception("Recovery authorization unexpectedly accepted."); }
    private sealed class Service : IInboxService, IDisposable
    {
        internal AuthorizationLease? Root = new();
        internal string? Key = new string('a', 64);
        internal int Lists, KeyReads;
        internal Action? OnList;
        internal AuthorizationLease? ObservedActor;
        internal IReadOnlyList<Mailbox> Boxes = [new(Own)];
        public AuthorizationLease? AcceptedAuthorization => Root;
        public string? RecoveryOwnerKey { get { KeyReads++; return Key; } }
        public Task<IReadOnlyList<Mailbox>> GetMailboxesAsync(CancellationToken ct)
        { Lists++; ObservedActor = AuthorizationContext.Current; OnList?.Invoke(); return Task.FromResult(Boxes); }
        public Task<InboxSnapshot> LoadAsync(string identity, CancellationToken ct) => throw new NotSupportedException();
        public Task<SendReceipt> SendAsync(string identity, string peer, string body, string? threadId, CancellationToken ct) => throw new NotSupportedException();
        public Task<string> CreateThreadAsync(string identity, string peer, string title, CancellationToken ct) => throw new NotSupportedException();
        public Task SetArchivedAsync(string identity, string peer, bool archived, CancellationToken ct) => throw new NotSupportedException();
        public Task AcknowledgeAsync(string identity, string messageId, CancellationToken ct) => throw new NotSupportedException();
        public Task SetContactAsync(string identity, Contact contact, CancellationToken ct) => throw new NotSupportedException();
        public Task RemoveContactAsync(string identity, string peer, CancellationToken ct) => throw new NotSupportedException();
        public Task DeleteThreadAsync(string identity, string threadId, CancellationToken ct) => throw new NotSupportedException();
        public void Dispose() => Root?.Retire();
    }
}
