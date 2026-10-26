using Pigeonpost.Core;

internal static class NativeCallSessionTests
{
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("A native call host refuses an unsigned session and requires explicit fixture opt-in", async () =>
        {
            var tokens = new Tokens { AcceptedAuthorization = null };
            try { using var invalid = new NativeCallSession(tokens, Mailbox); throw new Exception("Accepted an unsigned call host."); }
            catch (AuthorizationRetiredException) { }
            Require(tokens.Acquisitions == 0);
            using var fixture = new NativeCallSession(tokens, Mailbox, allowUnboundFixture: true);
            Require(await fixture.TokenAsync(false, default) == "synthetic-call-fixture" && fixture.IsActive);
            fixture.Dispose(); await Fails(() => fixture.TokenAsync(false, default));
        });
        yield return ("Late native-call token acquisition cannot cross accepted logins", async () =>
        {
            var tokens = new Tokens { Block = true }; using var host = new NativeCallSession(tokens, Mailbox);
            var old = host.TokenAsync(false, default); await tokens.Started.Task.WaitAsync(TimeSpan.FromSeconds(3));
            tokens.AcceptedAuthorization = new AuthorizationLease();
            tokens.Release.TrySetResult(); await Fails(() => old);
            Require(!host.IsActive && tokens.Acquisitions == 1);
        });
        yield return ("Closing a call host refuses its delayed renewal while preserving the account grant", async () =>
        {
            var tokens = new Tokens { Block = true }; using var host = new NativeCallSession(tokens, Mailbox);
            var old = host.TokenAsync(true, default); await tokens.Started.Task.WaitAsync(TimeSpan.FromSeconds(3));
            host.Dispose(); tokens.Release.TrySetResult(); await Fails(() => old);
            Require(tokens.AcceptedAuthorization!.IsActive && tokens.Renewals == 1);
        });
        yield return ("Native call renewal keeps a current login and canonical mailbox", async () =>
        {
            var tokens = new Tokens(); var accepted = tokens.AcceptedAuthorization;
            using var host = new NativeCallSession(tokens, Mailbox);
            Require(await host.TokenAsync(false, default) == "synthetic-call-fixture");
            Require(await host.TokenAsync(true, default) == "synthetic-call-fixture" && ReferenceEquals(accepted, tokens.AcceptedAuthorization));
            Require(host.Matches(Mailbox with { Handle = "/renamed/main" }) && !host.Matches(Mailbox with { Address = "/k/other" }));
            Require(host.ToString() == "NativeCallSession(redacted)");
        });
        yield return ("A native call's own actor cannot ignore retirement of its caller's child scope", async () =>
        {
            var tokens = new Tokens { Block = true }; using var host = new NativeCallSession(tokens, Mailbox);
            var caller = tokens.AcceptedAuthorization!.Child();
            using var scope = new AuthorizationContext(caller);
            var old = host.TokenAsync(false, default); await tokens.Started.Task.WaitAsync(TimeSpan.FromSeconds(3));
            caller.Retire(); tokens.Release.TrySetResult(); await Fails(() => old);
            Require(tokens.AcceptedAuthorization.IsActive);
        });
        yield return ("Cancelled native call work never acquires a token", async () =>
        {
            var tokens = new Tokens(); using var host = new NativeCallSession(tokens, Mailbox); using var cancelled = new CancellationTokenSource();
            cancelled.Cancel();
            try { await host.TokenAsync(false, cancelled.Token); throw new Exception("Accepted cancelled work."); }
            catch (OperationCanceledException) { }
            Require(tokens.Acquisitions == 0);
        });
        yield return ("Call-service credentials are limited to its route, production port and explicit local fixture origin", () =>
        {
            var endpoint = new Uri("http://127.0.0.1:9");
            Require(CallBridge.IsCallsService(new Uri("https://inbox.pigeonpost.dev/calls/v1/groups/g_fixture/call?from=%2Fk%2Fa"), endpoint));
            Require(CallBridge.IsCallsService(new Uri("http://127.0.0.1:9/calls/v1/groups/g_fixture/call"), endpoint));
            foreach (var value in new[] { "https://inbox.pigeonpost.dev:8443/calls/v1/groups/g_fixture/call", "http://inbox.pigeonpost.dev/calls/v1/groups/g_fixture/call",
                "https://user:password@inbox.pigeonpost.dev/calls/v1/groups/g_fixture/call", "https://inbox.pigeonpost.dev/calls/v1/groups/g_fixture/call#fragment",
                "https://inbox.pigeonpost.dev/other", "https://inbox.pigeonpost.dev/calls/v10/other", "https://other.example/calls/v1/groups/g_fixture/call",
                "http://127.0.0.1:8/calls/v1/groups/g_fixture/call", "http://localhost:9/calls/v1/groups/g_fixture/call" })
                Require(!CallBridge.IsCallsService(new Uri(value), endpoint));
            Require(!CallBridge.IsCallsService(new Uri("http://127.0.0.1:9/calls/v1/groups/g_fixture/call"), new Uri("https://postbox.pigeonpost.dev")));
            Require(!CallBridge.IsPage(new Uri("https://user:password@inbox.pigeonpost.dev/native-call.html"), CallBridge.ProductionPage));
            return Task.CompletedTask;
        });
    }
    private static readonly Mailbox Mailbox = new("/k/callfixture", "/caller/main");
    private static void Require(bool value) { if (!value) throw new Exception("Native call ownership assertion failed."); }
    private static async Task Fails(Func<Task> work)
    {
        try { await work(); } catch (AuthorizationRetiredException) { return; }
        throw new Exception("Expected the old call host to refuse.");
    }
    private sealed class Tokens : IAccessTokenProvider
    {
        public AuthorizationLease? AcceptedAuthorization { get; set; } = new();
        internal int Acquisitions, Renewals;
        internal bool Block;
        internal readonly TaskCompletionSource Started = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal readonly TaskCompletionSource Release = new(TaskCreationOptions.RunContinuationsAsynchronously);
        public async Task<string> GetTokenAsync(CancellationToken ct)
        {
            Acquisitions++; Started.TrySetResult();
            if (Block) await Release.Task; // deliberately ignores cancellation
            return "synthetic-call-fixture";
        }
        public Task<string> RefreshTokenAsync(CancellationToken ct) { Renewals++; return GetTokenAsync(ct); }
    }
}
