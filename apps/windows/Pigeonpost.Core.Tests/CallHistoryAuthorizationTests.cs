using System.Collections.Concurrent;
using System.Net;
using System.Text;
using System.Text.Json;
using Pigeonpost.Core;

internal static class CallHistoryAuthorizationTests
{
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("A queued clear-history continuation cannot clear the replacement inbox", async () =>
        {
            using var f = new Fixture(); var model = f.Model;
            model.Reset("/k/alice"); await model.LoadAsync();
            var hold = f.Handler.Pause(HttpMethod.Delete); var ui = new HeldUi();
            var old = ui.Capture(model.ClearAsync); await hold.Wait();
            hold.Release.TrySetResult(); await ui.Wait();
            model.Reset("/k/bob"); await model.LoadAsync();
            ui.Resume();
            Require(await old == "" && model.Entries.Single().Peer == "/k/bob");
        });
        yield return ("A queued delete-history continuation cannot remove the replacement inbox's call", async () =>
        {
            using var f = new Fixture(); var model = f.Model;
            model.Reset("/k/alice"); await model.LoadAsync();
            var entry = model.Entries.Single(); var hold = f.Handler.Pause(HttpMethod.Delete); var ui = new HeldUi();
            var old = ui.Capture(() => model.DeleteAsync(entry)); await hold.Wait();
            hold.Release.TrySetResult(); await ui.Wait();
            model.Reset("/k/bob"); await model.LoadAsync();
            ui.Resume();
            Require(await old == "" && model.Entries.Single().Peer == "/k/bob");
        });
        yield return ("Call history cannot publish or acquire a replacement login between its pages", async () =>
        {
            using var f = new Fixture(); var model = f.Model; model.Reset("/k/alice");
            var hold = f.Handler.Pause(HttpMethod.Get); var ui = new HeldUi();
            var old = ui.Capture(() => model.LoadAsync()); await hold.Wait();
            hold.Release.TrySetResult(); await ui.Wait();
            f.Tokens.AcceptedAuthorization = new AuthorizationLease(); // no controller reset and no retirement
            ui.Resume(); await old;
            Require(f.Tokens.Acquisitions == 1 && f.Handler.FilteredReads == 0 && model.Entries.Count == 0);
        });
        yield return ("An old calls-page action cannot borrow a newly accepted login", async () =>
        {
            using var f = new Fixture(); var model = f.Model;
            model.Reset("/k/alice"); await model.LoadAsync();
            var entry = model.Entries.Single(); var tokens = f.Tokens.Acquisitions;
            f.Tokens.AcceptedAuthorization!.Retire(); f.Tokens.AcceptedAuthorization = new AuthorizationLease();
            Require(await model.DeleteAsync(entry) == "" && f.Tokens.Acquisitions == tokens && f.Handler.Mutations == 0);
        });
        yield return ("Resetting a calls page clears the previous inbox's loading state", async () =>
        {
            using var f = new Fixture(); var model = f.Model; model.Reset("/k/alice");
            var hold = f.Handler.Pause(HttpMethod.Get); var old = model.LoadAsync(); await hold.Wait();
            Require(model.IsLoading);
            model.Reset("/k/bob"); Require(!model.IsLoading);
            hold.Release.TrySetResult(); await old; await model.LoadAsync();
            Require(!model.IsLoading && model.Entries.Single().Peer == "/k/bob");
        });
    }
    private static void Require(bool value) { if (!value) throw new Exception("Call history ownership assertion failed."); }
    private sealed class Tokens : IAccessTokenProvider
    {
        public AuthorizationLease? AcceptedAuthorization { get; set; } = new();
        internal int Acquisitions;
        public Task<string> GetTokenAsync(CancellationToken ct) { Acquisitions++; return Task.FromResult("synthetic-call-history-fixture"); }
        public Task<string> RefreshTokenAsync(CancellationToken ct) => GetTokenAsync(ct);
    }
    private sealed class Fixture : IDisposable
    {
        internal readonly Tokens Tokens = new();
        internal readonly History Handler = new();
        private readonly HttpClient http;
        private readonly PostboxClient postbox;
        internal readonly CallHistoryModel Model;
        internal Fixture()
        {
            http = new HttpClient(Handler); postbox = new PostboxClient(http, Tokens, new Uri("http://127.0.0.1:9"));
            Model = new CallHistoryModel(postbox);
        }
        public void Dispose() { Handler.Hold?.Release.TrySetResult(); Model.Reset(null); postbox.Dispose(); http.Dispose(); }
    }
    private sealed class Gate
    {
        internal readonly TaskCompletionSource Started = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal readonly TaskCompletionSource Release = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal Task Wait() => Started.Task.WaitAsync(TimeSpan.FromSeconds(3));
    }
    private sealed class History : HttpMessageHandler
    {
        internal Gate? Hold;
        private HttpMethod? method;
        private int held;
        internal int Mutations, FilteredReads;
        internal Gate Pause(HttpMethod wanted) { method = wanted; return Hold = new Gate(); }
        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken ct)
        {
            var identity = System.Web.HttpUtility.ParseQueryString(request.RequestUri!.Query)["identity"];
            object body;
            if (request.Method == HttpMethod.Get)
            {
                var filtered = System.Web.HttpUtility.ParseQueryString(request.RequestUri.Query)["filter"] == "filtered";
                if (filtered) FilteredReads++;
                CallLogEntry[] rows = filtered ? [] : [new() { Id = "call_shared", Peer = identity, Direction = "incoming", Kind = "audio", Outcome = "missed", StartedAt = 100 }];
                body = new { entries = rows, unseen_missed = rows.Length };
            }
            else { Mutations++; body = new { unseen_missed = 0 }; }
            var response = new HttpResponseMessage(HttpStatusCode.OK)
            {
                Content = new StringContent(JsonSerializer.Serialize(body, PostboxJson.Options), Encoding.UTF8, "application/json")
            };
            if (Hold is { } gate && request.Method == method && Interlocked.Exchange(ref held, 1) == 0)
            {
                gate.Started.TrySetResult(); await gate.Release.Task.ConfigureAwait(false);
            }
            return response;
        }
    }
    // The HTTP pipeline finishes first; only the model's UI continuation is held. Cancelling
    // HTTP after that point cannot protect a model that forgot to recheck its original actor.
    private sealed class HeldUi : SynchronizationContext
    {
        private readonly ConcurrentQueue<(SendOrPostCallback Callback, object? State)> callbacks = new();
        private readonly TaskCompletionSource queued = new(TaskCreationOptions.RunContinuationsAsynchronously);
        public override void Post(SendOrPostCallback callback, object? state) { callbacks.Enqueue((callback, state)); queued.TrySetResult(); }
        internal T Capture<T>(Func<T> work)
        {
            var previous = Current; SetSynchronizationContext(this);
            try { return work(); } finally { SetSynchronizationContext(previous); }
        }
        internal Task Wait() => queued.Task.WaitAsync(TimeSpan.FromSeconds(3));
        internal void Resume()
        {
            Capture(() => { while (callbacks.TryDequeue(out var row)) row.Callback(row.State); return 0; });
        }
    }
}
