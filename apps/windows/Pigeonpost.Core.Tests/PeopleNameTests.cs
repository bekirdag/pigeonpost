using System.Net;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Pigeonpost.Core;
using Pigeonpost.Core.Groups;

internal static class PeopleNameTests
{
    private const string Box = "/k/abc123", Peer = "/k/def456";
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Free names normalize Turkish and scalar lengths while refusing address and invisible impersonation", () =>
        {
            Require(PeopleNames.Normalize("  O\u0308zgür   Şahin  ") == "Özgür Şahin");
            Require(PeopleNames.Normalize(new string('a', 64), true).Length == 64);
            foreach (var input in new[] { "/marina", "@Marina", "Ad\u200bmin", "\u034fAlice", "Alice\ufe0f", "Pigeonpost team", "support", "---", new string('a', 41), "\ud800" })
                Fails<ArgumentException>(() => PeopleNames.Normalize(input));
            Require(PeopleNames.Normalize("Support", true) == "Support");
            return Task.CompletedTask;
        });
        yield return ("Local aliases override labels and names while self-set names remain distinguishable from handles", () =>
        {
            Require(new PeerName("My friend", "Marina").Resolve("Handle", "Owned inbox") == "My friend");
            Require(new PeerName(null, "Marina").Resolve("Handle", "Owned inbox") == "Owned inbox");
            Require(new PeerName(null, "Marina").Resolve("Handle") == "~Marina");
            Require(new PeerName().Resolve(Peer) == Peer);
            return Task.CompletedTask;
        });
        yield return ("Name lookup and writes carry the acting inbox and normalize the request without acquiring a handle", async () =>
        {
            using var tokens = new Tokens(); using var handler = new Handler(); using var http = new HttpClient(handler);
            using var client = new PostboxClient(http, tokens, new("http://127.0.0.1:9"));
            handler.Body = new { name = new { display_name = "Özgür", visibility = "known", updated_at = 1 } };
            var saved = await client.SetProfileNameAsync(Box, " O\u0308zgür ", "known", default);
            Require(saved.DisplayName == "Özgür" && handler.Path == "/v1/profile/name" && handler.Identity == Box && handler.Method == HttpMethod.Put);
            using var body = JsonDocument.Parse(handler.RequestBody!);
            Require(body.RootElement.GetProperty("display_name").GetString() == "Özgür");
            handler.Body = new { profiles = new Dictionary<string, object?> { [Peer] = new { address = Peer, handle = (string?)null, display_name = "Marina" } } };
            Require((await client.GetPeerProfilesAsync(Box, [Peer], default))[Peer]?.DisplayName == "Marina");
        });
        yield return ("Profile replies reject unsolicited identities, invalid names and missing fields", async () =>
        {
            using var tokens = new Tokens(); using var handler = new Handler(); using var http = new HttpClient(handler);
            using var client = new PostboxClient(http, tokens, new("http://127.0.0.1:9"));
            foreach (var response in new object[] {
                new { profiles = new Dictionary<string, object?> { ["/k/unasked"] = null } },
                new { profiles = new Dictionary<string, object?> { [Peer] = new { address = "/k/other", handle = (string?)null, display_name = "Marina" } } },
                new { profiles = new Dictionary<string, object?> { [Peer] = new { address = Peer, handle = (string?)null, display_name = "Admin" } } },
                new { profiles = new Dictionary<string, object?> { [Peer] = new { address = (string?)null, display_name = "Marina" } } } })
            { handler.Body = response; await FailsAsync<PostboxException>(() => client.GetPeerProfilesAsync(Box, [Peer], default)); }
            handler.Body = new { profiles = new Dictionary<string, object?> { [Peer] = null } };
            Require((await client.GetPeerProfilesAsync(Box, [Peer], default))[Peer] is null);
        });
        yield return ("Name API bounds inputs and response size without publishing a malformed result", async () =>
        {
            using var tokens = new Tokens(); using var handler = new Handler(); using var http = new HttpClient(handler);
            using var client = new PostboxClient(http, tokens, new("http://127.0.0.1:9"));
            await FailsAsync<ArgumentException>(() => client.GetPeerProfilesAsync(Box, [Peer, Peer], default));
            await FailsAsync<ArgumentException>(() => client.GetProfileNameAsync("/paid/handle", default));
            Require(handler.Calls == 0);
            handler.Body = new { padding = new string('a', 600 * 1024), name = (object?)null };
            await FailsAsync<HttpRequestException>(() => client.GetProfileNameAsync(Box, default));
        });
        yield return ("Retiring an account during name lookup discards its delayed reply", async () =>
        {
            using var tokens = new Tokens(); using var handler = new Handler { Hold = true }; using var http = new HttpClient(handler);
            using var client = new PostboxClient(http, tokens, new("http://127.0.0.1:9"));
            var pending = client.GetProfileNameAsync(Box, default); await handler.Started.Task;
            tokens.Root.Retire(); handler.Release.TrySetResult();
            await FailsAsync<AuthorizationRetiredException>(() => pending);
        });
        yield return ("Name labels cannot enter another inbox or a replacement login and do not grant contact permissions", async () =>
        {
            var root = new AuthorizationLease(); var preview = new PreviewInboxService(authorizationSource: () => root);
            using var vm = new InboxViewModel(preview); await vm.InitializeAsync();
            var peer = vm.AllConversations.First(c => !c.IsMine).Peer; var target = vm.CaptureNewConversationTarget()!;
            var contact = vm.ExactContact(peer);
            var names = new Dictionary<string, PeerName> { [peer] = new("Local friend", "Marina") };
            Require(vm.ApplyPeerNames(target, names) && vm.AllConversations.Single(c => c.Peer == peer).Name == "Local friend");
            names[peer] = new("Mutated caller");
            Require(vm.AllConversations.Single(c => c.Peer == peer).Name == "Local friend" && vm.ExactContact(peer) == contact);
            await vm.SwitchMailboxAsync(vm.Mailboxes[1]);
            Require(!vm.ApplyPeerNames(target, names) && vm.AllConversations.All(c => c.Name != "Local friend"));
            await vm.SwitchMailboxAsync(vm.Mailboxes[0]); Require(!vm.ApplyPeerNames(target, names));
            var next = vm.CaptureNewConversationTarget()!; root.Retire(); root = new(); await vm.InitializeAsync();
            Require(!vm.ApplyPeerNames(next, names)); root.Retire();
        });
        yield return ("Private aliases survive encrypted storage, are inbox/account scoped and clear on sign-out", async () =>
        {
            var folder = Path.Combine(Path.GetTempPath(), "pp-name-test-" + Guid.NewGuid().ToString("N"));
            var store = new LocalAliasFile(folder, new Protector());
            try
            {
                await store.SaveAsync("owner", "origin", Box, new Dictionary<string, string> { [Peer] = "Private friend" }, () => true);
                var bytes = File.ReadAllBytes(Directory.GetFiles(folder).Single());
                Require(!Encoding.UTF8.GetString(bytes).Contains("Private friend", StringComparison.Ordinal));
                Require((await store.LoadAsync("owner", "origin", Box, () => true))[Peer] == "Private friend");
                Require((await store.LoadAsync("other", "origin", Box, () => true)).Count == 0);
                Require((await store.LoadAsync("owner", "other-origin", Box, () => true)).Count == 0);
                Require((await store.LoadAsync("owner", "origin", Peer, () => true)).Count == 0);
                store.Clear(); Require(!Directory.Exists(folder));
            }
            finally { store.Clear(); }
        });
        yield return ("An alias save finishing encryption after sign-out cannot recreate the deleted file", async () =>
        {
            var folder = Path.Combine(Path.GetTempPath(), "pp-name-test-" + Guid.NewGuid().ToString("N")); var protector = new Protector { Hold = true };
            var store = new LocalAliasFile(folder, protector); var live = true;
            try
            {
                var saving = store.SaveAsync("owner", "origin", Box, new Dictionary<string, string> { [Peer] = "Private friend" }, () => live);
                await protector.Started.Task; live = false; store.Clear(); protector.Release.TrySetResult();
                await FailsAsync<AuthorizationRetiredException>(() => saving); Require(!Directory.Exists(folder));
            }
            finally { store.Clear(); }
        });
    }
    private static void Require(bool condition) { if (!condition) throw new Exception("People names assertion failed."); }
    private static void Fails<T>(Action action) where T : Exception
    { try { action(); } catch (T) { return; } throw new Exception("Expected refusal."); }
    private static async Task FailsAsync<T>(Func<Task> action) where T : Exception
    { try { await action(); } catch (T) { return; } throw new Exception("Expected refusal."); }
    private sealed class Tokens : IAccessTokenProvider, IDisposable
    {
        internal readonly AuthorizationLease Root = new();
        public AuthorizationLease? AcceptedAuthorization => Root;
        public Task<string> GetTokenAsync(CancellationToken ct) => Task.FromResult("synthetic-name-test-token");
        public Task<string> RefreshTokenAsync(CancellationToken ct) => GetTokenAsync(ct);
        public void Dispose() => Root.Retire();
    }
    private sealed class Handler : HttpMessageHandler
    {
        internal object Body = new { name = (object?)null };
        internal string? Path, Identity, RequestBody;
        internal HttpMethod? Method; internal int Calls; internal bool Hold;
        internal readonly TaskCompletionSource Started = new(TaskCreationOptions.RunContinuationsAsynchronously), Release = new(TaskCreationOptions.RunContinuationsAsynchronously);
        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken ct)
        {
            Calls++; Path = request.RequestUri!.AbsolutePath; Method = request.Method;
            Identity = request.Headers.GetValues("x-pigeonpost-identity").Single();
            RequestBody = request.Content is null ? null : await request.Content.ReadAsStringAsync(ct);
            Started.TrySetResult(); if (Hold) await Release.Task;
            return new(HttpStatusCode.OK) { Content = new StringContent(JsonSerializer.Serialize(Body), Encoding.UTF8, "application/json") };
        }
    }
    private sealed class Protector : IStateProtector
    {
        private readonly byte[] key = RandomNumberGenerator.GetBytes(32);
        internal bool Hold;
        internal readonly TaskCompletionSource Started = new(TaskCreationOptions.RunContinuationsAsynchronously), Release = new(TaskCreationOptions.RunContinuationsAsynchronously);
        public async Task<byte[]> ProtectAsync(byte[] plain)
        {
            Started.TrySetResult(); if (Hold) await Release.Task;
            var nonce = RandomNumberGenerator.GetBytes(12); var result = new byte[28 + plain.Length]; nonce.CopyTo(result, 0);
            using var aes = new AesGcm(key, 16); aes.Encrypt(nonce, plain, result.AsSpan(28), result.AsSpan(12, 16)); return result;
        }
        public Task<byte[]> UnprotectAsync(byte[] bytes)
        {
            var plain = new byte[bytes.Length - 28]; using var aes = new AesGcm(key, 16);
            aes.Decrypt(bytes.AsSpan(0, 12), bytes.AsSpan(28), bytes.AsSpan(12, 16), plain); return Task.FromResult(plain);
        }
    }
}
