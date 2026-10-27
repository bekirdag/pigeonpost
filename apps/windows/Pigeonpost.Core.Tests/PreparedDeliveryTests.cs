using System.Net;
using System.Text;
using System.Text.Json;
using Pigeonpost.Core;
using Pigeonpost.Core.Groups;

internal static class PreparedDeliveryTests
{
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("A restarted client explicitly retries the exact protected request bytes without a new reference", async () =>
        {
            var folder = Path.Combine(Path.GetTempPath(), "pp-prepared-wire-" + Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(folder);
            try
            {
                using var f = new Fixture(); using var scope = await f.Scope(); var protector = new Protector();
                var store = new FileMessageAttemptStore(folder, protector);
                var original = PreparedMessageAttempt.Create(scope, new(Own, Peer, "Fixture payload \"quoted\" ğ 😀", replyTo: "parent_copy", quote: "Fixture quote"));
                await store.SaveAsync(scope, original); f.Handler.LoseNext = true;
                await Fails<HttpRequestException>(() => f.Client.SendPreparedMessageAsync(scope, original, default));
                Require(f.Handler.Bodies.Count == 1); f.Tokens.Root.Retire();
                var newTokens = new Tokens(); using var restarted = new PostboxClient(f.Http, newTokens, new("http://127.0.0.1:9"));
                using var current = await MessageRecoveryScope.AuthorizeAsync(restarted, Own) ?? throw new Exception("Fixture scope missing.");
                var loaded = await new FileMessageAttemptStore(folder, protector).LoadAsync(current, Peer, null) ?? throw new Exception("Fixture attempt missing.");
                var receipt = await restarted.SendPreparedMessageAsync(current, loaded, default);
                Require(receipt.MsgRef == original.Request.MsgRef && f.Handler.Bodies.Count == 2 && f.Handler.Bodies[0] == f.Handler.Bodies[1]);
                Require(await store.RemoveAsync(current, Peer, null, receipt.MsgRef!)); newTokens.Root.Retire();
            }
            finally { Directory.Delete(folder, recursive: true); }
        });
        yield return ("Prepared requests preserve the original caller through later asynchronous token acquisition", async () =>
        {
            using var f = new Fixture(); var caller = f.Tokens.Root.Child(); MessageRecoveryScope scope;
            using (new AuthorizationContext(caller)) scope = await f.Scope();
            using (scope)
            {
                var request = PreparedMessageAttempt.Create(scope, new(Own, Peer, "Retired caller fixture"));
                f.Tokens.Block = true; var before = f.Handler.Calls;
                var send = f.Client.SendPreparedMessageAsync(scope, request, default);
                await f.Tokens.Waiting.Task.WaitAsync(TimeSpan.FromSeconds(3)); caller.Retire(); f.Tokens.Release.TrySetResult();
                await Fails<AuthorizationRetiredException>(() => send);
                Require(f.Handler.Calls == before && f.Handler.Bodies.Count == 0 && f.Tokens.Root.IsActive);
            }
        });
        yield return ("Prepared delivery refuses an older or disabled message-action server before POST", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope(); f.Handler.Features = [];
            var failure = await Fails<PostboxException>(() => f.Client.SendPreparedMessageAsync(scope, PreparedMessageAttempt.Create(scope, new(Own, Peer, "Old server fixture")), default));
            Require(failure.Code == "feature_disabled" && f.Handler.Bodies.Count == 0);
            f.Handler.Features = [ServerFeatures.MessageReply];
            failure = await Fails<PostboxException>(() => f.Client.SendPreparedMessageAsync(scope, PreparedMessageAttempt.Create(scope, new(Own, Peer, "Edit disabled fixture", editOf: "parent_copy")), default));
            Require(failure.Code == "feature_disabled" && f.Handler.Bodies.Count == 0);
        });
        yield return ("A recovery scope cannot be used through a different postbox client even with the same credentials", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            using var other = new PostboxClient(f.Http, f.Tokens, new("http://127.0.0.1:9")); var before = f.Handler.Calls;
            await Fails<AuthorizationRetiredException>(() => other.SendPreparedMessageAsync(scope, PreparedMessageAttempt.Create(scope, new(Own, Peer, "Different client fixture")), default));
            Require(f.Handler.Calls == before);
        });
    }
    private const string Own = "/k/aaaaaaaaaaaaaaaaaaaaaaaaaa", Peer = "/k/bbbbbbbbbbbbbbbbbbbbbbbbbb";
    private static void Require(bool value) { if (!value) throw new Exception("Prepared delivery assertion failed."); }
    private static async Task<T> Fails<T>(Func<Task> action) where T : Exception
    { try { await action(); } catch (T error) { return error; } throw new Exception("Prepared delivery unexpectedly accepted."); }
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
            return "synthetic-prepared-delivery-token";
        }
        public Task<string> RefreshTokenAsync(CancellationToken ct) => GetTokenAsync(ct);
    }
    private sealed class Handler : HttpMessageHandler
    {
        internal readonly List<string> Bodies = [];
        internal string[] Features = [ServerFeatures.MessageEdit, ServerFeatures.MessageReply, ServerFeatures.Receipts];
        internal int Calls;
        internal bool LoseNext;
        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken ct)
        {
            Calls++;
            object response = request.RequestUri!.AbsolutePath switch
            {
                "/v1/identities" => new { identities = new[] { new { address = Own } } },
                "/v1/whoami" => new { address = Own },
                "/v1/capabilities" => new { features = Features },
                _ => new { }
            };
            if (request.Method == HttpMethod.Post && request.RequestUri.AbsolutePath == "/v1/send")
            {
                var json = await request.Content!.ReadAsStringAsync(ct); Bodies.Add(json);
                if (LoseNext) { LoseNext = false; throw new HttpRequestException("Synthetic response loss."); }
                using var body = JsonDocument.Parse(json);
                response = new { message_id = "recipient_copy", sent_copy_id = "sender_copy", msg_ref = body.RootElement.GetProperty("msg_ref").GetString(), duplicate = Bodies.Count > 1 };
            }
            return new(HttpStatusCode.OK) { Content = new StringContent(JsonSerializer.Serialize(response), Encoding.UTF8, "application/json") };
        }
    }
    private sealed class Fixture : IDisposable
    {
        internal readonly Tokens Tokens = new(); internal readonly Handler Handler = new(); internal readonly HttpClient Http;
        internal readonly PostboxClient Client;
        internal Fixture() { Http = new(Handler); Client = new(Http, Tokens, new("http://127.0.0.1:9")); }
        internal async Task<MessageRecoveryScope> Scope() => await MessageRecoveryScope.AuthorizeAsync(Client, Own) ?? throw new Exception("Fixture scope missing.");
        public void Dispose() { Tokens.Release.TrySetResult(); Client.Dispose(); Http.Dispose(); Tokens.Root.Retire(); }
    }
    // Fixed reversible protection is restricted to this transport fixture. Cryptographic storage
    // is independently exercised with AES-GCM in MessageAttemptStoreTests; Windows uses DPAPI.
    private sealed class Protector : IStateProtector
    {
        public Task<byte[]> ProtectAsync(byte[] bytes) => Task.FromResult(bytes.Select(b => (byte)(b ^ 0xa5)).ToArray());
        public Task<byte[]> UnprotectAsync(byte[] bytes) => ProtectAsync(bytes);
    }
}
