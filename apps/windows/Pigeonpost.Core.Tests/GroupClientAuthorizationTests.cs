using System.Net;
using System.Security.Cryptography;
using System.Text;
using Pigeonpost.Core;
using Pigeonpost.Core.Groups;
using Pigeonpost.Core.Preview;

internal static class GroupClientAuthorizationTests
{
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("A bound group API cannot adopt another accepted login", async () =>
        {
            using var f = new Fixture();
            var group = f.Device(f.Alice);
            await group.InitAsync(default);
            var calls = f.Tokens.Acquisitions;
            // Defend even against a stand-in provider that forgot to retire its old grant.
            f.Tokens.AcceptedAuthorization = new AuthorizationLease();
            await Fails<AuthorizationRetiredException>(() => group.Api.ListAsync(default));
            Require(f.Tokens.Acquisitions == calls);
        });
        yield return ("Late group registration and queued work drain safely after disposal", async () =>
        {
            foreach (var acceptedLogin in new[] { true, false })
            {
                using var f = new Fixture();
                if (!acceptedLogin) f.Tokens.AcceptedAuthorization = null;
                var store = new MemoryGroupStateStore();
                var group = f.Device(f.Alice, store);
                f.Handler.PausePath = "/v1/mls/devices"; f.Handler.PauseMethod = HttpMethod.Post;
                var pending = group.InitAsync(default);
                await f.Handler.Started.Task.WaitAsync(TimeSpan.FromSeconds(3));
                var queued = group.TopUpKeyPackagesAsync(default);
                group.Dispose(); group.Dispose();
                f.Handler.Release.TrySetResult();
                await Fails<AuthorizationRetiredException>(() => pending);
                await Fails<AuthorizationRetiredException>(() => queued);
                Require(await store.LoadAsync(f.Alice) is null);
                await Fails<AuthorizationRetiredException>(() => Task.FromResult(group.HistoryOf("old")));
                Require(!f.Server.Requests.Contains("POST /v1/mls/key-packages"));
            }
        });
        yield return ("A delayed group-state load cannot expose plaintext or erase a replacement", async () =>
        {
            using var f = new Fixture();
            var saved = new MemoryGroupStateStore();
            using (var original = f.Device(f.Alice, saved))
            {
                await original.InitAsync(default);
                var gid = (await original.CreateGroupAsync(new GroupSettings("Private fixture"), [], default)).Group.Id;
                await original.SendAsync(gid, "Private cached fixture", [], default);
            }
            var paused = new PausedLoad(saved);
            var old = f.Device(f.Alice, paused);
            var pending = old.InitAsync(default);
            await paused.Started.Task.WaitAsync(TimeSpan.FromSeconds(3));
            old.Dispose();
            var replacement = new GroupDeviceRecord { DeviceId = "replacement" };
            await saved.SaveAsync(f.Alice, replacement);
            paused.Release.TrySetResult();
            await Fails<AuthorizationRetiredException>(() => pending);
            Require((await saved.LoadAsync(f.Alice))!.DeviceId == "replacement");
            Require(paused.Saves == 0);
        });
        yield return ("A late encrypted group save cannot replace the new client's device", async () =>
        {
            using var f = new Fixture();
            var folder = Path.Combine(Path.GetTempPath(), "pp-actor-" + Guid.NewGuid().ToString("N"));
            try
            {
                var key = new FixedKeyProtector(RandomNumberGenerator.GetBytes(32));
                var protector = new PausedProtection(key);
                var old = f.Device(f.Alice, new FileGroupStateStore(folder, protector));
                var pending = old.InitAsync(default);
                await protector.Started.Task.WaitAsync(TimeSpan.FromSeconds(3));
                old.Dispose();
                var currentStore = new FileGroupStateStore(folder, key);
                var current = f.Device(f.Alice, currentStore);
                var id = await current.InitAsync(default);
                protector.Release.TrySetResult();
                await Fails<AuthorizationRetiredException>(() => pending);
                Require((await currentStore.LoadAsync(f.Alice))!.DeviceId == id);
                Require(!Directory.EnumerateFiles(folder, "*.tmp").Any());
            }
            finally { if (Directory.Exists(folder)) Directory.Delete(folder, true); }
        });
        yield return ("A mailbox must be verified before its encrypted cache is opened", async () =>
        {
            using var f = new Fixture(); var store = new PausedLoad(new MemoryGroupStateStore());
            f.Handler.WrongWhoAmI = f.Bob;
            var group = f.Device(f.Alice, store);
            var error = await Fails<PostboxException>(() => group.InitAsync(default));
            Require(error.Code == "wrong_identity" && !store.Started.Task.IsCompleted);
            Require(!f.Server.Requests.Contains("POST /v1/mls/devices"));
        });
        yield return ("An encrypted device from another mailbox cannot open or replace this mailbox's history", async () =>
        {
            using var f = new Fixture(); var store = new MemoryGroupStateStore();
            var original = f.Device(f.Alice, store); await original.InitAsync(default);
            var gid = (await original.CreateGroupAsync(new GroupSettings("Alice fixture"), [], default)).Group.Id;
            await original.SendAsync(gid, "Alice private fixture", [], default);
            var saved = (await store.LoadAsync(f.Alice))!;
            await store.SaveAsync(f.Bob, saved);
            var registrations = f.Server.Requests.Count(x => x == "POST /v1/mls/devices");
            var wrong = f.Device(f.Bob, store);
            await Fails<GroupStateUnavailableException>(() => wrong.InitAsync(default));
            Require(wrong.HistoryOf(gid).Count == 0);
            Require(f.Server.Requests.Count(x => x == "POST /v1/mls/devices") == registrations);
            Require((await store.LoadAsync(f.Bob))!.DeviceId == saved.DeviceId);
        });
        yield return ("A revoked cached device refuses history before any automatic enrollment", async () =>
        {
            using var f = new Fixture(); var store = new MemoryGroupStateStore();
            var original = f.Device(f.Alice, store); var id = await original.InitAsync(default);
            var gid = (await original.CreateGroupAsync(new GroupSettings("Revocation fixture"), [], default)).Group.Id;
            await original.SendAsync(gid, "Cached fixture", [], default);
            original.Dispose(); f.Server.RevokeDevice(f.Alice, id);
            var registrations = f.Server.Requests.Count(x => x == "POST /v1/mls/devices");
            var again = f.Device(f.Alice, store);
            var error = await Fails<PostboxException>(() => again.InitAsync(default));
            Require(error.Code == "device_revoked" && again.HistoryOf(gid).Count == 0);
            Require(f.Server.Requests.Count(x => x == "POST /v1/mls/devices") == registrations);
            Require((await store.LoadAsync(f.Alice))!.DeviceId == id);
        });
        yield return ("Missing or unknown group membership never enables private actions", () =>
        {
            var missing = new GroupInfo { Id = "g_fixture", ActivationState = "active", MembersCanInvite = true, MembersCanChangeTimer = true };
            foreach (var value in new[] { missing, missing with { My = new GroupMy { Role = "member" } },
                missing with { My = new GroupMy { Role = "unknown", State = "active" } },
                missing with { My = new GroupMy { Role = "owner", State = "unknown" } },
                missing with { ActivationState = "provisioning", My = new GroupMy { Role = "owner", State = "active" } } })
                Require(!value.IsActive && !value.CanPost && !value.CanInvite && !value.CanChangeTimer);
            Require((missing with { My = new GroupMy { Role = "member", State = "active" } }).IsActive);
            return Task.CompletedTask;
        });
        yield return ("Signing in again to the same mailbox rebinds the group page and clears its draft", async () =>
        {
            using var f = new Fixture(); var store = new MemoryGroupStateStore();
            await SeedGroups(f, store, "First");
            using var model = new GroupsModel(f.Postbox, _ => store);
            await Activate(model, f); await model.SelectAsync(model.Groups.Single());
            model.Draft = "Old private draft";
            model.AddFile(new GroupClient.OutgoingFile("fixture.txt", "text/plain", [1]));
            var verified = f.Server.Requests.Count(x => x == "GET /v1/whoami");
            f.Tokens.AcceptedAuthorization!.Retire(); f.Tokens.AcceptedAuthorization = new AuthorizationLease();
            await Activate(model, f);
            Require(f.Server.Requests.Count(x => x == "GET /v1/whoami") == verified + 1);
            Require(model.Draft == "" && model.DraftFiles.Count == 0 && model.Selected is null && model.Lines.Count == 0);
            Require(model.DeviceFingerprint is not null && model.Error is null && model.Groups.Count == 1);
        });
        yield return ("A group page verifies current membership before showing cached history", async () =>
        {
            using var f = new Fixture(); var store = new MemoryGroupStateStore();
            var gid = (await SeedGroups(f, store, "Private"))[0];
            using var model = new GroupsModel(f.Postbox, _ => store); await Activate(model, f);
            var row = model.Groups.Single();
            Require(row.Preview.Contains("Private fixture", StringComparison.Ordinal));
            f.Handler.RefusedGroup = gid;
            await model.SelectAsync(row);
            Require(model.Selected is null && model.Lines.Count == 0 && model.Groups.Count == 0 && !model.CanSend && !model.CanCall);
        });
        yield return ("A delayed group selection cannot replace the more recent selection", async () =>
        {
            using var f = new Fixture(); var store = new MemoryGroupStateStore();
            var ids = await SeedGroups(f, store, "First", "Second");
            using var model = new GroupsModel(f.Postbox, _ => store); await Activate(model, f);
            f.Handler.PausePath = "/v1/groups/" + ids[0];
            var old = model.SelectAsync(model.Groups.Single(x => x.Id == ids[0]));
            await f.Handler.Started.Task.WaitAsync(TimeSpan.FromSeconds(3));
            await model.SelectAsync(model.Groups.Single(x => x.Id == ids[1]));
            f.Handler.Release.TrySetResult(); await old;
            Require(model.Selected?.Id == ids[1] && model.Lines.Any(x => x.Text == "Second fixture"));
            Require(!model.Lines.Any(x => x.Text == "First fixture"));
        });
        yield return ("A retired group refresh cannot block or change the replacement page", async () =>
        {
            using var f = new Fixture(); var store = new MemoryGroupStateStore();
            await SeedGroups(f, store, "Current");
            using var model = new GroupsModel(f.Postbox, _ => store); await Activate(model, f);
            f.Handler.PausePath = "/v1/mls/key-packages/count";
            var old = model.RefreshAsync();
            await f.Handler.Started.Task.WaitAsync(TimeSpan.FromSeconds(3));
            f.Tokens.AcceptedAuthorization!.Retire(); f.Tokens.AcceptedAuthorization = new AuthorizationLease();
            await Activate(model, f);
            model.Draft = "Replacement draft";
            f.Handler.Release.TrySetResult(); await old;
            var requests = f.Server.Requests.Count(x => x == "GET /v1/mls/key-packages/count");
            await model.RefreshAsync();
            Require(f.Server.Requests.Count(x => x == "GET /v1/mls/key-packages/count") > requests);
            Require(model.Draft == "Replacement draft" && model.Error is null && model.Groups.Count == 1);
        });
        yield return ("A late group send cannot clear a replacement login's draft", async () =>
        {
            using var f = new Fixture(); var store = new MemoryGroupStateStore();
            var gid = (await SeedGroups(f, store, "Current"))[0];
            using var model = new GroupsModel(f.Postbox, _ => store); await Activate(model, f);
            await model.SelectAsync(model.Groups.Single()); model.Draft = "Old accepted send";
            f.Handler.PausePath = "/v1/groups/" + gid + "/messages"; f.Handler.PauseMethod = HttpMethod.Post;
            var old = model.SendAsync();
            await f.Handler.Started.Task.WaitAsync(TimeSpan.FromSeconds(3));
            f.Tokens.AcceptedAuthorization!.Retire(); f.Tokens.AcceptedAuthorization = new AuthorizationLease();
            await Activate(model, f); await model.SelectAsync(model.Groups.Single());
            model.Draft = "Replacement draft";
            f.Handler.Release.TrySetResult();
            Require(!await old && model.Draft == "Replacement draft" && !model.IsSending && model.Error is null);
        });
    }

    private static Task Activate(GroupsModel model, Fixture f)
    {
        var mailbox = new Mailbox(f.Alice, "/alice/main");
        return model.ActivateAsync(mailbox, new ServerFeatures(new HashSet<string> { ServerFeatures.Groups }), [mailbox], []);
    }
    private static async Task<string[]> SeedGroups(Fixture f, MemoryGroupStateStore store, params string[] names)
    {
        using var seed = f.Device(f.Alice, store); await seed.InitAsync(default);
        var ids = new List<string>();
        foreach (var name in names)
        {
            var gid = (await seed.CreateGroupAsync(new GroupSettings(name), [], default)).Group.Id;
            await seed.SendAsync(gid, name + " fixture", [], default); ids.Add(gid);
        }
        return ids.ToArray();
    }

    private static void Require(bool value) { if (!value) throw new Exception("Group ownership assertion failed."); }
    private static async Task<T> Fails<T>(Func<Task> work) where T : Exception
    {
        try { await work(); } catch (T error) { return error; }
        throw new Exception("Expected " + typeof(T).Name);
    }
    private sealed class Tokens : IAccessTokenProvider
    {
        public AuthorizationLease? AcceptedAuthorization { get; set; } = new();
        public int Acquisitions;
        public Task<string> GetTokenAsync(CancellationToken ct) { Acquisitions++; return Task.FromResult("synthetic-group-fixture"); }
        public Task<string> RefreshTokenAsync(CancellationToken ct) => GetTokenAsync(ct);
    }
    private sealed class Fixture : IDisposable
    {
        internal readonly PreviewPostbox Server = new();
        internal readonly Tokens Tokens = new();
        internal readonly HeldResponse Handler;
        private readonly HttpClient http;
        internal readonly PostboxClient Postbox;
        internal readonly string Alice;
        internal readonly string Bob;
        private readonly List<GroupClient> clients = [];
        internal Fixture()
        {
            Alice = Server.AddMailbox(PreviewPostbox.Seed(141), "/alice/main");
            Bob = Server.AddMailbox(PreviewPostbox.Seed(142), "/bob/main");
            Handler = new HeldResponse(Server); http = new HttpClient(Handler);
            Postbox = new PostboxClient(http, Tokens, new Uri("http://127.0.0.1:9"));
        }
        internal GroupClient Device(string mailbox, IGroupStateStore? store = null)
        {
            var client = new GroupClient(new GroupApi(Postbox, mailbox), store ?? new MemoryGroupStateStore(), sleep: _ => Task.CompletedTask, jitter: () => 0);
            clients.Add(client); return client;
        }
        public void Dispose()
        {
            Handler.Release.TrySetResult();
            foreach (var client in clients) client.Dispose();
            Postbox.Dispose(); http.Dispose(); Server.Dispose();
        }
    }
    private sealed class HeldResponse(HttpMessageHandler server) : HttpMessageHandler
    {
        private readonly HttpMessageInvoker inner = new(server, false);
        private int held;
        internal string? PausePath;
        internal HttpMethod PauseMethod = HttpMethod.Get;
        internal string? WrongWhoAmI;
        internal string? RefusedGroup;
        internal TaskCompletionSource Started = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal TaskCompletionSource Release = new(TaskCreationOptions.RunContinuationsAsynchronously);
        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken ct)
        {
            if (request.RequestUri!.AbsolutePath == "/v1/whoami" && WrongWhoAmI is { } other)
                return new HttpResponseMessage(HttpStatusCode.OK) { Content = new StringContent("{\"address\":\"" + other + "\"}", Encoding.UTF8, "application/json") };
            if (request.Method == HttpMethod.Get && RefusedGroup is { } gid && request.RequestUri.AbsolutePath == "/v1/groups/" + gid)
                return new HttpResponseMessage(HttpStatusCode.Forbidden) { Content = new StringContent("{\"error\":\"not_member\"}", Encoding.UTF8, "application/json") };
            var response = await inner.SendAsync(request, ct);
            if (request.RequestUri.AbsolutePath == PausePath && request.Method == PauseMethod && Interlocked.Exchange(ref held, 1) == 0)
            {
                Started.TrySetResult(); await Release.Task; // deliberately ignores cancellation
            }
            return response;
        }
        protected override void Dispose(bool disposing) { if (disposing) inner.Dispose(); base.Dispose(disposing); }
    }
    private sealed class PausedLoad(MemoryGroupStateStore inner) : IAuthorizedGroupStateStore
    {
        public bool Persistent => false;
        internal readonly TaskCompletionSource Started = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal readonly TaskCompletionSource Release = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal int Saves;
        public Task<GroupDeviceRecord?> LoadAsync(string mailbox) => inner.LoadAsync(mailbox);
        public Task SaveAsync(string mailbox, GroupDeviceRecord record) => inner.SaveAsync(mailbox, record);
        public Task RemoveAsync(string mailbox) => inner.RemoveAsync(mailbox);
        public async Task<GroupDeviceRecord?> LoadAsync(string mailbox, AuthorizationLease actor)
        {
            var value = await inner.LoadAsync(mailbox, actor); Started.TrySetResult(); await Release.Task; return value;
        }
        public Task SaveAsync(string mailbox, GroupDeviceRecord record, AuthorizationLease actor) { Saves++; return inner.SaveAsync(mailbox, record, actor); }
        public Task RemoveAsync(string mailbox, AuthorizationLease actor) => inner.RemoveAsync(mailbox, actor);
    }
    private sealed class PausedProtection(IStateProtector inner) : IStateProtector
    {
        internal readonly TaskCompletionSource Started = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal readonly TaskCompletionSource Release = new(TaskCreationOptions.RunContinuationsAsynchronously);
        public async Task<byte[]> ProtectAsync(byte[] bytes)
        {
            var result = await inner.ProtectAsync(bytes); Started.TrySetResult(); await Release.Task; return result;
        }
        public Task<byte[]> UnprotectAsync(byte[] bytes) => inner.UnprotectAsync(bytes);
    }
}
