using System.Net;
using System.Net.Http.Headers;
using System.Text;
using Pigeonpost.Core;

internal static class AuthorizationTests
{
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Child authorization contexts survive awaits without changing their parent", async () =>
        {
            var root = new AuthorizationLease(); var child = root.Child(); var sibling = root.Child();
            using (new AuthorizationContext(root))
            {
                await Task.Run(async () =>
                {
                    using (new AuthorizationContext(child))
                    {
                        await Task.Yield(); Require(ReferenceEquals(AuthorizationContext.Current, child));
                    }
                    Require(ReferenceEquals(AuthorizationContext.Current, root));
                });
                Require(ReferenceEquals(AuthorizationContext.Current, root));
            }
            Require(AuthorizationContext.Current is null && child.SharesAuthorization(root));
            child.Retire(); Require(!child.IsActive && sibling.IsActive && root.IsActive);
            root.Retire(); Require(!sibling.IsActive && !root.IsActive);
            await Fails<AuthorizationRetiredException>(() => { sibling.WithActive(() => 1); return Task.CompletedTask; });
        });
        yield return ("Retirement stops new writes and waits for an admitted local write", async () =>
        {
            var root = new AuthorizationLease(); var started = Signal(); var release = Signal(); var wrote = false;
            var writer = Task.Run(() => root.WithActive(() =>
            {
                started.SetResult(); release.Task.GetAwaiter().GetResult(); wrote = true; return true;
            }));
            await started.Task.WaitAsync(TimeSpan.FromSeconds(5));
            var retiring = Task.Run(root.Retire);
            try
            {
                Require(SpinWait.SpinUntil(() => !root.IsActive, TimeSpan.FromSeconds(5)));
                Require(!retiring.IsCompleted);
                await Fails<AuthorizationRetiredException>(() => { root.WithActive(() => false); return Task.CompletedTask; });
            }
            finally { release.TrySetResult(); }
            await Task.WhenAll(writer, retiring).WaitAsync(TimeSpan.FromSeconds(5)); Require(wrote);
        });
        yield return ("A delayed token acquisition cannot send as a replacement account", async () =>
        {
            var tokens = new Tokens { Acquiring = Signal(), ReleaseAcquire = Signal() }; var requests = 0;
            using var http = new HttpClient(new Handler((_, _) => { requests++; return Task.FromResult(Json("{\"messages\":[]}")); }));
            using var api = Client(http, tokens);
            var pending = api.GetInboxAsync("/k/fixture");
            await tokens.Acquiring.Task.WaitAsync(TimeSpan.FromSeconds(5));
            tokens.Replace(retire: false); tokens.ReleaseAcquire.SetResult();
            await Fails<AuthorizationRetiredException>(() => pending); Require(requests == 0);
        });
        yield return ("A rejected write cannot renew and replay under a replacement account", async () =>
        {
            var tokens = new Tokens { Renewing = Signal(), ReleaseRenew = Signal() }; var requests = 0;
            using var http = new HttpClient(new Handler((_, _) =>
            {
                requests++; return Task.FromResult(Json("{\"error\":\"unauthorized\"}", HttpStatusCode.Unauthorized));
            }));
            using var api = Client(http, tokens);
            var pending = api.SendAsync("/k/fixture", "/fixture/peer", "Synthetic write", null, default);
            await tokens.Renewing.Task.WaitAsync(TimeSpan.FromSeconds(5));
            tokens.Replace(); tokens.ReleaseRenew.SetResult();
            await Fails<AuthorizationRetiredException>(() => pending); Require(requests == 1);
        });
        yield return ("Late JSON and binary body completions cannot return old private data", async () =>
        {
            foreach (var binary in new[] { false, true })
            {
                var tokens = new Tokens();
                var content = new DeferredContent(Encoding.UTF8.GetBytes(binary ? "Synthetic bytes" : "{\"messages\":[]}"));
                using var http = new HttpClient(new Handler((_, _) => Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK) { Content = content })));
                using var api = Client(http, tokens);
                Task pending = binary ? api.DownloadAsync("/k/fixture", "file_fixture", default) : api.GetInboxAsync("/k/fixture");
                await content.Started.Task.WaitAsync(TimeSpan.FromSeconds(5));
                tokens.Replace(retire: false); content.Release.SetResult();
                await Fails<AuthorizationRetiredException>(() => pending);
            }
        });
        yield return ("Mailbox enrichment cannot borrow a new login between its requests", async () =>
        {
            var tokens = new Tokens(); var requests = 0;
            using var http = new HttpClient(new Handler((_, _) =>
            {
                requests++;
                return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK)
                {
                    Content = new SwitchOnDisposeContent("{\"identities\":[{\"address\":\"/k/fixture\",\"label\":\"Synthetic\"}]}", () => tokens.Replace(retire: false))
                });
            }));
            using var api = Client(http, tokens);
            await Fails<AuthorizationRetiredException>(() => api.GetMailboxesAsync(default)); Require(requests == 1);
        });
        yield return ("An operation keeps its reviewed actor after an earlier request completed", async () =>
        {
            var tokens = new Tokens(); var actor = tokens.AcceptedAuthorization!.Child(); var requests = 0;
            using var http = new HttpClient(new Handler((_, _) => { requests++; return Task.FromResult(Json("{\"messages\":[]}")); }));
            using var api = Client(http, tokens);
            using (new AuthorizationContext(actor))
            {
                await api.GetInboxAsync("/k/fixture"); tokens.Replace(retire: false);
                await Fails<AuthorizationRetiredException>(() => api.GetInboxAsync("/k/fixture"));
            }
            Require(requests == 1);
        });
        yield return ("An old account stream cannot invoke a new account's refresh callback", async () =>
        {
            var tokens = new Tokens(); var callbacks = 0; var requests = 0;
            var content = new DeferredContent(Encoding.UTF8.GetBytes("event: mail\nid: 1\n\n"));
            content.Headers.ContentType = new MediaTypeHeaderValue("text/event-stream");
            using var http = new HttpClient(new Handler((_, _) => { requests++; return Task.FromResult(new HttpResponseMessage(HttpStatusCode.OK) { Content = content }); }));
            using var api = Client(http, tokens);
            var pending = api.WatchMailAsync(() => { callbacks++; return Task.CompletedTask; }, default);
            await content.Started.Task.WaitAsync(TimeSpan.FromSeconds(5));
            tokens.Replace(); content.Release.SetResult();
            await Fails<AuthorizationRetiredException>(() => pending); Require(callbacks == 0 && requests == 1);
        });
        yield return ("An inner actor cannot ignore retirement of its caller's independent scope", async () =>
        {
            var tokens = new Tokens { Acquiring = Signal(), ReleaseAcquire = Signal() }; var requests = 0;
            var caller = tokens.AcceptedAuthorization!.Child(); var actor = tokens.AcceptedAuthorization.Child();
            using var http = new HttpClient(new Handler((_, _) => { requests++; return Task.FromResult(Json("{\"messages\":[]}")); }));
            using var api = Client(http, tokens);
            using (new AuthorizationContext(caller))
            using (new AuthorizationContext(actor))
            {
                var pending = api.GetInboxAsync("/k/fixture"); await tokens.Acquiring.Task.WaitAsync(TimeSpan.FromSeconds(5));
                caller.Retire(); tokens.ReleaseAcquire.SetResult();
                await Fails<AuthorizationRetiredException>(() => pending);
            }
            Require(actor.IsActive && tokens.AcceptedAuthorization.IsActive && requests == 0);
        });
        yield return ("A provider cannot silently substitute another grant under the same root", async () =>
        {
            var tokens = new Tokens { Acquiring = Signal(), ReleaseAcquire = Signal() }; var requests = 0;
            var original = tokens.AcceptedAuthorization!;
            using var http = new HttpClient(new Handler((_, _) => { requests++; return Task.FromResult(Json("{\"messages\":[]}")); }));
            using var api = Client(http, tokens);
            var pending = api.GetInboxAsync("/k/fixture"); await tokens.Acquiring.Task.WaitAsync(TimeSpan.FromSeconds(5));
            tokens.NarrowWithoutRetiring(); tokens.ReleaseAcquire.SetResult();
            await Fails<AuthorizationRetiredException>(() => pending);
            Require(original.IsActive && tokens.AcceptedAuthorization!.SharesAuthorization(original) && requests == 0);
        });
    }
    private static PostboxClient Client(HttpClient http, Tokens tokens) => new(http, tokens, new Uri("http://127.0.0.1:1/"));
    private static TaskCompletionSource Signal() => new(TaskCreationOptions.RunContinuationsAsynchronously);
    private static HttpResponseMessage Json(string body, HttpStatusCode status = HttpStatusCode.OK) => new(status) { Content = new StringContent(body, Encoding.UTF8, "application/json") };
    private static void Require(bool condition) { if (!condition) throw new Exception("Authorization behavior assertion failed."); }
    private static async Task Fails<T>(Func<Task> action) where T : Exception
    {
        try { await action().WaitAsync(TimeSpan.FromSeconds(5)); }
        catch (T) { return; }
        throw new Exception("Expected " + typeof(T).Name);
    }
    private sealed class Tokens : IAccessTokenProvider
    {
        public AuthorizationLease? AcceptedAuthorization { get; private set; } = new();
        public TaskCompletionSource? Acquiring, ReleaseAcquire, Renewing, ReleaseRenew;
        public async Task<string> GetTokenAsync(CancellationToken cancellationToken)
        {
            Acquiring?.TrySetResult(); if (ReleaseAcquire is not null) await ReleaseAcquire.Task;
            return "fixture-access";
        }
        public async Task<string> RefreshTokenAsync(CancellationToken cancellationToken)
        {
            Renewing?.TrySetResult(); if (ReleaseRenew is not null) await ReleaseRenew.Task;
            return "fixture-renewed";
        }
        public void Replace(bool retire = true)
        {
            if (retire) AcceptedAuthorization?.Retire();
            AcceptedAuthorization = new AuthorizationLease();
        }
        public void NarrowWithoutRetiring() => AcceptedAuthorization = AcceptedAuthorization!.Child();
    }
    private sealed class DeferredContent(byte[] bytes) : HttpContent
    {
        public readonly TaskCompletionSource Started = Signal(), Release = Signal();
        protected override bool TryComputeLength(out long length) { length = bytes.Length; return true; }
        protected override async Task SerializeToStreamAsync(Stream stream, TransportContext? context)
        {
            Started.TrySetResult(); await Release.Task; await stream.WriteAsync(bytes);
        }
    }
    private sealed class SwitchOnDisposeContent(string body, Action changed) : StringContent(body, Encoding.UTF8, "application/json")
    {
        private int disposed;
        protected override void Dispose(bool disposing)
        {
            base.Dispose(disposing);
            if (disposing && Interlocked.Exchange(ref disposed, 1) == 0) changed();
        }
    }
}
