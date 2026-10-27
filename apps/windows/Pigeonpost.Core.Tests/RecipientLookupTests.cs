using System.Net;
using System.Text;
using System.Text.Json;
using Pigeonpost.Core;

internal static class RecipientLookupTests
{
    private const string Own = "/k/aaaaaaaaaaaaaaaaaaaaaaaaaa", Peer = "/k/bbbbbbbbbbbbbbbbbbbbbbbbbb";
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Recipient lookup sends the reviewed handle with the original freshly authorized canonical sender", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            var route = await f.Client.ResolveRecipientAsync(scope, "/fixture/main", default);
            Require(route.From == Own && route.Address == Peer && route.Handle == "/fixture/main");
            using var body = JsonDocument.Parse(f.Handler.LookupBodies.Single());
            Require(body.RootElement.GetProperty("identity").GetString() == Own && body.RootElement.GetProperty("to").GetString() == "/fixture/main"
                && body.RootElement.EnumerateObject().Count() == 2 && f.Handler.Deliveries == 0);
        });
        yield return ("Recipient lookup refuses unknown or disabled capabilities before its POST", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope(); f.Handler.Enabled = false;
            var error = await Fails<PostboxException>(() => f.Client.ResolveRecipientAsync(scope, "/fixture/main", default));
            Require(error.Code == "feature_disabled" && f.Handler.LookupBodies.Count == 0 && f.Handler.Deliveries == 0);
        });
        yield return ("Recipient lookup rejects a substituted sender, alias recipient and unsafe returned handle", async () =>
        {
            foreach (var response in new object[] {
                new { from = Peer, address = Peer, handle = "/fixture/main" },
                new { from = Own, address = "/fixture/main", handle = "/fixture/main" },
                new { from = Own, address = Peer, handle = "/fixture\nprivate" },
                new { from = Own, address = (string?)null, handle = "/fixture/main" } })
            {
                using var f = new Fixture(); using var scope = await f.Scope(); f.Handler.Route = response;
                var error = await Fails<PostboxException>(() => f.Client.ResolveRecipientAsync(scope, "/fixture/main", default));
                Require(error.Code == "bad_response" && f.Handler.Deliveries == 0);
            }
        });
        yield return ("Recipient lookup validates bounded reviewed input before any network request", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope(); var before = f.Handler.Calls;
            foreach (var input in new[] { "not an address", "/fixture\nprivate", "/" + new string('a', 513) })
                await Fails<ArgumentException>(() => f.Client.ResolveRecipientAsync(scope, input, default));
            Require(f.Handler.Calls == before && f.Handler.LookupBodies.Count == 0);
        });
        yield return ("Recipient lookup preserves original caller retirement through delayed token acquisition", async () =>
        {
            using var f = new Fixture(); var caller = f.Tokens.Root.Child(); MessageRecoveryScope scope;
            using (new AuthorizationContext(caller)) scope = await f.Scope();
            using (scope)
            {
                var before = f.Handler.Calls; f.Tokens.Block = true;
                var lookup = f.Client.ResolveRecipientAsync(scope, "/fixture/main", default);
                await f.Tokens.Waiting.Task.WaitAsync(TimeSpan.FromSeconds(3)); caller.Retire(); f.Tokens.Release.TrySetResult();
                await Fails<AuthorizationRetiredException>(() => lookup);
                Require(f.Handler.Calls == before && f.Tokens.Root.IsActive);
            }
        });
        yield return ("An account retired during recipient lookup cannot publish the response", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope(); f.Handler.Block = true;
            var lookup = f.Client.ResolveRecipientAsync(scope, "/fixture/main", default);
            await f.Handler.Waiting.Task.WaitAsync(TimeSpan.FromSeconds(3)); f.Tokens.Root.Retire(); f.Handler.Release.TrySetResult();
            await Fails<AuthorizationRetiredException>(() => lookup);
            Require(f.Handler.Deliveries == 0);
        });
        yield return ("Recipient lookup rejects a scope from a different client before network access", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            using var other = new PostboxClient(f.Http, f.Tokens, new("http://127.0.0.1:9")); var before = f.Handler.Calls;
            await Fails<AuthorizationRetiredException>(() => other.ResolveRecipientAsync(scope, "/fixture/main", default));
            Require(f.Handler.Calls == before);
        });
    }
    private static void Require(bool value) { if (!value) throw new Exception("Recipient lookup assertion failed."); }
    private static async Task<T> Fails<T>(Func<Task> action) where T : Exception
    { try { await action(); } catch (T ex) { return ex; } throw new Exception("Recipient lookup unexpectedly accepted."); }
    private sealed class Tokens : IAccessTokenProvider
    {
        internal readonly AuthorizationLease Root = new();
        public AuthorizationLease? AcceptedAuthorization => Root;
        public string? RecoveryOwnerKey => new string('a', 64);
        internal bool Block;
        internal readonly TaskCompletionSource Waiting = new(TaskCreationOptions.RunContinuationsAsynchronously), Release = new(TaskCreationOptions.RunContinuationsAsynchronously);
        public async Task<string> GetTokenAsync(CancellationToken ct)
        {
            if (Block) { Block = false; Waiting.TrySetResult(); await Release.Task.ConfigureAwait(false); }
            return "synthetic-recipient-lookup-token";
        }
        public Task<string> RefreshTokenAsync(CancellationToken ct) => GetTokenAsync(ct);
    }
    private sealed class Handler : HttpMessageHandler
    {
        internal int Calls, Deliveries;
        internal bool Enabled = true, Block;
        internal readonly List<string> LookupBodies = [];
        internal object Route = new { from = Own, address = Peer, handle = "/fixture/main" };
        internal readonly TaskCompletionSource Waiting = new(TaskCreationOptions.RunContinuationsAsynchronously), Release = new(TaskCreationOptions.RunContinuationsAsynchronously);
        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken ct)
        {
            Calls++;
            object response = request.RequestUri!.AbsolutePath switch {
                "/v1/identities" => new { identities = new[] { new { address = Own } } },
                "/v1/whoami" => new { address = Own },
                "/v1/capabilities" => new { features = Enabled ? new[] { ServerFeatures.RecipientLookup } : [] },
                _ => new { }
            };
            if (request.RequestUri.AbsolutePath == "/v1/recipients/resolve")
            {
                LookupBodies.Add(await request.Content!.ReadAsStringAsync(ct));
                if (Block) { Waiting.TrySetResult(); await Release.Task.ConfigureAwait(false); }
                response = Route;
            }
            if (request.RequestUri.AbsolutePath == "/v1/send") Deliveries++;
            return new(HttpStatusCode.OK) { Content = new StringContent(JsonSerializer.Serialize(response), Encoding.UTF8, "application/json") };
        }
    }
    private sealed class Fixture : IDisposable
    {
        internal readonly Tokens Tokens = new(); internal readonly Handler Handler = new(); internal readonly HttpClient Http;
        internal readonly PostboxClient Client;
        internal Fixture() { Http = new(Handler); Client = new(Http, Tokens, new("http://127.0.0.1:9")); }
        internal async Task<MessageRecoveryScope> Scope() => await MessageRecoveryScope.AuthorizeAsync(Client, Own) ?? throw new Exception("Fixture scope missing.");
        public void Dispose() { Tokens.Release.TrySetResult(); Handler.Release.TrySetResult(); Client.Dispose(); Http.Dispose(); Tokens.Root.Retire(); }
    }
}
