using System.Collections;
using System.Net;
using System.Text;
using System.Text.Json;
using Pigeonpost.Core;

internal static class AvatarAuthorizationTests
{
    private const string Alice = "/alice/main";
    private const string Bob = "/bob/main";
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("An old avatar lookup cannot refill a cleared cache or release its replacement lookup", async () =>
        {
            using var f = new Fixture(); var first = f.Handler.HoldLookup(); var second = f.Handler.HoldLookup();
            var old = f.Directory.RefreshAsync([Alice], default); await first.Wait();
            f.Directory.Clear();
            var current = f.Directory.RefreshAsync([Alice], default); await second.Wait();
            first.Release.TrySetResult(); await old;
            Require(f.Directory.Known(Alice) is null);
            await f.Directory.RefreshAsync([Alice], default);
            Require(f.Handler.Lookups == 2);
            second.Release.TrySetResult(); await current;
            Require(f.Directory.Known(Alice) is not null);
        });
        yield return ("An old avatar download cannot replace the current bytes or remove its download task", async () =>
        {
            using var f = new Fixture(); await f.Directory.RefreshAsync([Alice], default);
            var first = f.Handler.HoldImage(); var second = f.Handler.HoldImage();
            var old = f.Directory.ImageAsync(Alice, default); await first.Wait();
            f.Directory.Clear(); await f.Directory.RefreshAsync([Alice], default);
            f.Handler.Image = [9, 8, 7];
            var current = f.Directory.ImageAsync(Alice, default); await second.Wait();
            first.Release.TrySetResult(); Require(await old is null);
            Require(ReferenceEquals(f.Directory.ImageAsync(Alice, default), current) && f.Handler.Images == 2);
            second.Release.TrySetResult(); Require((await current)!.SequenceEqual(new byte[] { 9, 8, 7 }));
            Require((await f.Directory.ImageAsync(Alice, default))!.SequenceEqual(new byte[] { 9, 8, 7 }) && f.Handler.Images == 2);
        });
        yield return ("Avatar metadata and cached bytes belong to their original accepted login", async () =>
        {
            using var f = new Fixture(); var first = f.Handler.HoldLookup();
            var old = f.Directory.RefreshAsync([Alice], default); await first.Wait();
            f.Tokens.AcceptedAuthorization!.Retire(); f.Tokens.AcceptedAuthorization = new AuthorizationLease();
            Require(f.Directory.Known(Alice) is null);
            f.Handler.Hash = "replacement";
            await f.Directory.RefreshAsync([Alice], default);
            first.Release.TrySetResult(); await old;
            Require(f.Directory.Known(Alice)?.Hash == "replacement");
            await f.Directory.ImageAsync(Alice, default);
            f.Tokens.AcceptedAuthorization = new AuthorizationLease(); // also defend a non-retiring stand-in
            Require(f.Directory.Known(Alice) is null && await f.Directory.ImageAsync(Alice, default) is null);
        });
        yield return ("Disabling profile pictures retires pending downloads and enabling starts a fresh cache", async () =>
        {
            using var f = new Fixture(); await f.Directory.RefreshAsync([Alice], default);
            var first = f.Handler.HoldImage(); var old = f.Directory.ImageAsync(Alice, default); await first.Wait();
            f.Directory.Enabled = false;
            first.Release.TrySetResult(); Require(await old is null && f.Directory.Known(Alice) is null);
            f.Directory.Enabled = true; await f.Directory.RefreshAsync([Alice], default);
            Require(await f.Directory.ImageAsync(Alice, default) is not null && f.Handler.Images == 2);
        });
        yield return ("Forgetting a picture retires pending results while retaining unrelated authorized images", async () =>
        {
            using var f = new Fixture(); await f.Directory.RefreshAsync([Bob], default);
            var expected = await f.Directory.ImageAsync(Bob, default);
            var first = f.Handler.HoldLookup(); var old = f.Directory.RefreshAsync([Alice], default); await first.Wait();
            var revision = f.Directory.Version;
            f.Directory.Forget([Alice]);
            first.Release.TrySetResult(); await old;
            Require(f.Directory.Known(Alice) is null && f.Directory.Known(Bob) is not null);
            Require((await f.Directory.ImageAsync(Bob, default))!.SequenceEqual(expected!) && f.Handler.Images == 1);
            var writes = 0;
            Require(!f.Directory.TryWithCurrent(revision, () => writes++));
            Require(f.Directory.TryWithCurrent(f.Directory.Version, () => writes++) && writes == 1);
            f.Tokens.AcceptedAuthorization!.Retire();
            Require(!f.Directory.TryWithCurrent(f.Directory.Version, () => writes++) && writes == 1);
        });
        yield return ("A multi-batch avatar lookup cannot acquire a replacement login between batches", async () =>
        {
            using var f = new Fixture();
            var addresses = new SwitchingAddresses(() =>
            {
                f.Tokens.AcceptedAuthorization!.Retire(); f.Tokens.AcceptedAuthorization = new AuthorizationLease();
            });
            try { await f.Postbox.LookupAvatarsAsync(addresses, default); throw new Exception("Old lookup adopted a replacement login."); }
            catch (AuthorizationRetiredException) { }
            Require(f.Handler.Lookups == 1 && f.Tokens.Acquisitions == 1);
        });
        yield return ("An unchanged avatar revision cannot admit callbacks from a retired caller scope", async () =>
        {
            using var f = new Fixture(); await f.Directory.RefreshAsync([Alice], default);
            var revision = f.Directory.Version; var caller = f.Tokens.AcceptedAuthorization!.Child();
            using var operation = new AuthorizationContext(caller);
            Require(f.Directory.IsCurrent(revision));
            caller.Retire();
            Require(!f.Directory.IsCurrent(revision) && f.Tokens.AcceptedAuthorization.IsActive);
            var wrote = false;
            Require(!f.Directory.TryWithCurrent(revision, () => wrote = true) && !wrote);
        });
    }

    private static void Require(bool value) { if (!value) throw new Exception("Avatar ownership assertion failed."); }
    private sealed class Tokens : IAccessTokenProvider
    {
        public AuthorizationLease? AcceptedAuthorization { get; set; } = new();
        internal int Acquisitions;
        public Task<string> GetTokenAsync(CancellationToken ct) { Acquisitions++; return Task.FromResult("synthetic-avatar-fixture"); }
        public Task<string> RefreshTokenAsync(CancellationToken ct) => GetTokenAsync(ct);
    }
    private sealed class Fixture : IDisposable
    {
        internal readonly Tokens Tokens = new();
        internal readonly Assets Handler = new();
        private readonly HttpClient http;
        internal readonly PostboxClient Postbox;
        internal readonly AvatarDirectory Directory;
        internal Fixture()
        {
            http = new HttpClient(Handler); Postbox = new PostboxClient(http, Tokens, new Uri("http://127.0.0.1:9"));
            Directory = new AvatarDirectory(Postbox) { Enabled = true };
        }
        public void Dispose() { Handler.ReleaseAll(); Directory.Clear(); Postbox.Dispose(); http.Dispose(); }
    }
    private sealed class Hold
    {
        internal readonly TaskCompletionSource Started = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal readonly TaskCompletionSource Release = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal Task Wait() => Started.Task.WaitAsync(TimeSpan.FromSeconds(3));
    }
    private sealed class Assets : HttpMessageHandler
    {
        private readonly Queue<Hold> lookups = new();
        private readonly Queue<Hold> images = new();
        private readonly List<Hold> all = [];
        internal string Hash = "fixture";
        internal byte[] Image = [1, 2, 3];
        internal int Lookups, Images;
        internal Hold HoldLookup() { var hold = new Hold(); lookups.Enqueue(hold); all.Add(hold); return hold; }
        internal Hold HoldImage() { var hold = new Hold(); images.Enqueue(hold); all.Add(hold); return hold; }
        internal void ReleaseAll() { foreach (var hold in all) hold.Release.TrySetResult(); }
        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken ct)
        {
            HttpResponseMessage response; Hold? hold;
            if (request.RequestUri!.AbsolutePath == "/v1/avatars")
            {
                Lookups++;
                var keys = (System.Web.HttpUtility.ParseQueryString(request.RequestUri.Query)["addresses"] ?? "").Split(',');
                var info = new AvatarInfo("av_fixture", 1, Hash, new AvatarUrls { Jpeg = "/v1/avatar-blobs/fixture/1/256.jpg" });
                var json = JsonSerializer.Serialize(new { avatars = keys.ToDictionary(key => key, _ => info) }, PostboxJson.Options);
                response = new HttpResponseMessage(HttpStatusCode.OK) { Content = new StringContent(json, Encoding.UTF8, "application/json") };
                hold = lookups.TryDequeue(out var pending) ? pending : null;
            }
            else if (request.RequestUri.AbsolutePath.StartsWith("/v1/avatar-blobs/", StringComparison.Ordinal))
            {
                Images++; response = new HttpResponseMessage(HttpStatusCode.OK) { Content = new ByteArrayContent(Image.ToArray()) };
                hold = images.TryDequeue(out var pending) ? pending : null;
            }
            else throw new Exception("Unexpected synthetic avatar route.");
            if (hold is not null) { hold.Started.TrySetResult(); await hold.Release.Task; } // noncooperative by design
            return response;
        }
    }
    private sealed class SwitchingAddresses(Action switchLogin) : IReadOnlyCollection<string>
    {
        public int Count => 101;
        public IEnumerator<string> GetEnumerator()
        {
            for (var i = 0; i < Count; i++) { if (i == 100) switchLogin(); yield return "/fixture/" + i; }
        }
        IEnumerator IEnumerable.GetEnumerator() => GetEnumerator();
    }
}
