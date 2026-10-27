using System.Net.Http.Headers;
using System.Net.Http.Json;
using System.Text.Json;
using Pigeonpost.Core;
using Pigeonpost.Core.Enterprise;
using Pigeonpost.Core.Groups;

// Actual room wire contract, protected creation custody and real MLS, against only the explicit
// disposable loopback fixture (tests/e2e-messages/windows.sh <binary> orgs).
internal static class OrganizationRoomsPostbox
{
    internal static async Task<int> RunAsync()
    {
        var stage = "setup";
        var folder = Path.Combine(Path.GetTempPath(), "pp-windows-rooms-postbox-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(folder);
        try
        {
            var file = Environment.GetEnvironmentVariable("PIGEONPOST_TEST_FIXTURE") ?? throw new InvalidOperationException();
            using var setup = JsonDocument.Parse(await File.ReadAllTextAsync(file)); var root = setup.RootElement;
            var endpoint = Local(root.GetProperty("postbox").GetString()!); var proxy = Local(root.GetProperty("lossProxy").GetString()!);
            var org = root.GetProperty("organizationId").GetString()!; Check(OrganizationRoomInput.Identifier(org));
            var people = root.GetProperty("people");
            string Get(string who, string field) => people.GetProperty(who).GetProperty(field).GetString()!;
            var aliceMailbox = Get("alice", "address"); var bobMailbox = Get("bob", "address");
            using var http = new HttpClient(new HttpClientHandler { AllowAutoRedirect = false }) { Timeout = TimeSpan.FromSeconds(20) };
            var aliceTokens = new Tokens(Get("alice", "token"), 'a'); var bobTokens = new Tokens(Get("bob", "token"), 'b');
            using var alice = new PostboxClient(http, aliceTokens, endpoint);
            using var bob = new PostboxClient(http, bobTokens, endpoint);
            using var carol = new PostboxClient(http, new Tokens(Get("carol", "token"), 'c'), endpoint);
            using var lossy = new PostboxClient(http, aliceTokens, proxy);

            stage = "canonical membership and personal-inbox separation";
            var aliceCatalog = await alice.OrganizationsAsync(aliceMailbox, default);
            var aliceOrg = aliceCatalog.OwnedFor(aliceMailbox).Single();
            var bobOrg = (await bob.OrganizationsAsync(bobMailbox, default)).OwnedFor(bobMailbox).Single();
            Check(aliceOrg.Id == org && bobOrg.Id == org && aliceOrg.MayCreate(aliceMailbox) && aliceCatalog.Delegated.Count == 0);
            Check((await bob.OrganizationsAsync(bobMailbox, default)).OwnedFor(Get("bob", "personalAddress")).Count == 0);
            await Denied(() => bob.RoomsAsync(Get("bob", "personalAddress"), org, "", default));
            await Denied(() => carol.RoomsAsync(Get("carol", "address"), org, "", default));

            stage = "real MLS device registration";
            var aliceGroups = new GroupClient(new GroupApi(alice, aliceMailbox), new MemoryGroupStateStore(), sleep: _ => Task.CompletedTask, jitter: () => 0);
            var bobGroups = new GroupClient(new GroupApi(bob, bobMailbox), new MemoryGroupStateStore(), sleep: _ => Task.CompletedTask, jitter: () => 0);
            await aliceGroups.InitAsync(default); await bobGroups.InitAsync(default);
            await aliceGroups.TopUpKeyPackagesAsync(default); await bobGroups.TopUpKeyPackagesAsync(default);

            stage = "store creation with a lost committed reply, protected custody and restart";
            using var protector = new MessageAttemptStoreTests.Protector();
            OrganizationRoomStore Store(PostboxClient api, Tokens tokens, string mailbox) =>
                new(api, api, new FileRoomCreationStore(folder, protector), () => new(tokens.RecoveryOwnerKey!, tokens.AcceptedAuthorization!, mailbox));
            var first = Store(lossy, aliceTokens, aliceMailbox); first.Synchronize();
            await Done(first.LoadOrganizations());
            Check(first.State.Organization?.Id == org && first.State.CustodyChecked && first.CanCreate(first.State.ContextId) && first.State.PendingCreation is null);
            Check(first.BeginCreation(first.State.ContextId));
            first.ChangeDraft(first.State.Draft! with { Name = "Loopback design", Description = "Synthetic room ğ" }, first.State.ContextId);
            await Control(http, proxy, "after");
            await Done(first.SubmitDraft(first.State.Draft!.Id, first.State.ContextId));
            Check(first.State.PendingCreation is { CandidateId: null } && first.State.CompletedCreation is null && first.State.Message!.Contains("not confirmed"));
            var lostKey = first.State.PendingCreation!.Payload.RequestKey;
            Check((await AttemptRows(http, proxy)).Length == 1);
            first.Dispose();
            var restarted = Store(lossy, aliceTokens, aliceMailbox); restarted.Synchronize();
            await Done(restarted.LoadOrganizations());
            Check(restarted.State.PendingCreation?.Payload.RequestKey == lostKey && restarted.State.Draft?.Name == "Loopback design");
            await Control(http, proxy, "off");
            await Done(restarted.SubmitDraft(restarted.State.Draft!.Id, restarted.State.ContextId));
            var created = restarted.State.CompletedCreation ?? throw new InvalidOperationException();
            Check(restarted.State.PendingCreation is null && restarted.State.Opening?.GroupId == created.Id && created.Role == "owner" && created.OrganizationId == org);
            var attempts = await AttemptRows(http, proxy);
            Check(attempts.Length == 2 && attempts.All(a => a.GetProperty("requestKey").GetString() == lostKey && a.GetProperty("status").GetInt32() is >= 200 and < 300)
                && attempts.Select(a => a.GetProperty("sha").GetString()).Distinct().Count() == 1);
            var again = Store(lossy, aliceTokens, aliceMailbox); again.Synchronize(); await Done(again.LoadOrganizations());
            Check(again.State.PendingCreation is null);

            stage = "real MLS activation and one directory row";
            await aliceGroups.ActivateAsync(created.Id, default);
            Check((await alice.RoomsAsync(aliceMailbox, org, "Loopback design", default)).Count(r => r.Id == created.Id) == 1);

            stage = "explicit reviewed public join and verified encrypted membership";
            var bobStore = Store(bob, bobTokens, bobMailbox); bobStore.Synchronize();
            await Done(bobStore.LoadOrganizations(search: "Loopback design"));
            var row = bobStore.State.Rooms.Single(r => r.Id == created.Id); Check(row.Action == RoomAction.Join);
            await Done(bobStore.Inspect(row.Id, bobStore.State.DirectoryProof, bobStore.State.ContextId));
            Check(bobStore.ReviewAction(bobStore.State.ReviewProof, bobStore.State.ContextId) == RoomAction.Join);
            await Done(bobStore.OpenReviewed(bobStore.State.ReviewProof, bobStore.State.ContextId));
            Check(bobStore.State.Opening?.GroupId == created.Id && (await bob.RoomAsync(bobMailbox, created.Id, default)).State == "keying");
            Check(await aliceGroups.ProcessPendingAsync(created.Id, default, noJitter: true) is not null);
            Check((await bobGroups.ProcessWelcomesAsync(default)).Contains(created.Id));
            Check(aliceGroups.SafetyNumberOf(created.Id) is { } safety && safety == bobGroups.SafetyNumberOf(created.Id));
            await aliceGroups.SendAsync(created.Id, "Synthetic organization message", [], default); await bobGroups.SyncAsync(created.Id, default);
            Check(bobGroups.HistoryOf(created.Id).Any(l => l.Text == "Synthetic organization message"));

            stage = "private room invitation and explicit acceptance";
            Check(again.BeginCreation(again.State.ContextId));
            again.ChangeDraft(again.State.Draft! with { Name = "Loopback private", Join = RoomJoinPolicy.Invited }, again.State.ContextId);
            await Done(again.SubmitDraft(again.State.Draft!.Id, again.State.ContextId));
            var privateRoom = again.State.CompletedCreation ?? throw new InvalidOperationException();
            await aliceGroups.ActivateAsync(privateRoom.Id, default);
            await new GroupApi(alice, aliceMailbox).InviteAsync(privateRoom.Id, [bobMailbox], default);
            await Done(bobStore.LoadRooms("Loopback private"));
            var invited = bobStore.State.Rooms.Single(r => r.Id == privateRoom.Id); Check(invited.Action == RoomAction.AcceptInvitation);
            await Done(bobStore.Inspect(invited.Id, bobStore.State.DirectoryProof, bobStore.State.ContextId));
            await Done(bobStore.OpenReviewed(bobStore.State.ReviewProof, bobStore.State.ContextId));
            Check(bobStore.State.Opening?.GroupId == privateRoom.Id && (await bob.RoomAsync(bobMailbox, privateRoom.Id, default)).OrganizationId == org);
            Check(await aliceGroups.ProcessPendingAsync(privateRoom.Id, default, noJitter: true) is not null && (await bobGroups.ProcessWelcomesAsync(default)).Contains(privateRoom.Id));
            Check(aliceGroups.SafetyNumberOf(privateRoom.Id) == bobGroups.SafetyNumberOf(privateRoom.Id));

            stage = "live offboarding refuses old room metadata";
            using (var remove = new HttpRequestMessage(HttpMethod.Post, new Uri(endpoint, "/v1/orgs/" + org + "/members/" + bobOrg.MemberId + "/remove")))
            {
                remove.Headers.Authorization = new AuthenticationHeaderValue("Bearer", Get("alice", "token"));
                remove.Content = JsonContent.Create(new { });
                using var removed = await http.SendAsync(remove); Check(removed.IsSuccessStatusCode);
            }
            await Denied(() => bob.RoomAsync(bobMailbox, created.Id, default));
            await Denied(() => bob.RoomsAsync(bobMailbox, org, "", default));
            Check((await bob.OrganizationsAsync(bobMailbox, default)).OwnedFor(bobMailbox).Count == 0);
            await Done(bobStore.LoadRooms(""), allowNull: true);
            Check(bobStore.State.Rooms.Count == 0);
            Console.WriteLine("PASS Windows organization-room interoperability: canonical catalog, scoped directory, protected lost-reply creation custody across restart with exact retry, MLS activation, reviewed public join, private acceptance and offboarding.");
            return 0;
        }
        catch (Exception error)
        { Console.Error.WriteLine($"FAIL Windows organization-room interoperability at {stage} ({error.GetType().Name}: {(error is PostboxException p ? p.StatusCode + " " + p.Code : "")})."); return 1; }
        finally { Directory.Delete(folder, recursive: true); }
    }
    private static async Task Done(Task? task, bool allowNull = false) { if (task is null) { Check(allowNull); return; } await task.WaitAsync(TimeSpan.FromSeconds(30)); }
    private static async Task Denied(Func<Task> work)
    {
        try { await work(); } catch (PostboxException failure) when (failure.StatusCode is 403 or 404) { return; }
        throw new InvalidOperationException("Missing organization authority was accepted.");
    }
    private static Uri Local(string value)
    {
        var uri = new Uri(value);
        if (!uri.IsLoopback || uri.Scheme != "http" || uri.AbsolutePath != "/" || uri.UserInfo.Length > 0 || uri.Query.Length > 0 || uri.Fragment.Length > 0) throw new InvalidOperationException();
        return uri;
    }
    private static void Check(bool value) { if (!value) throw new InvalidOperationException("Assertion failed."); }
    private static async Task Control(HttpClient http, Uri proxy, string mode)
    { using var response = await http.PostAsJsonAsync(new Uri(proxy, "/__test/control"), new { mode }); response.EnsureSuccessStatusCode(); }
    private static async Task<JsonElement[]> AttemptRows(HttpClient http, Uri proxy)
    {
        using var response = await http.GetAsync(new Uri(proxy, "/__test/attempts")); response.EnsureSuccessStatusCode();
        using var document = JsonDocument.Parse(await response.Content.ReadAsStringAsync());
        return document.RootElement.GetProperty("attempts").EnumerateArray().Select(e => e.Clone()).ToArray();
    }
    private sealed class Tokens(string token, char owner) : IAccessTokenProvider
    {
        public AuthorizationLease? AcceptedAuthorization { get; } = new();
        public string? RecoveryOwnerKey => new string(owner, 64);
        public Task<string> GetTokenAsync(CancellationToken ct) { ct.ThrowIfCancellationRequested(); return Task.FromResult(token); }
        public Task<string> RefreshTokenAsync(CancellationToken ct) => throw new InvalidOperationException("Synthetic fixture does not renew.");
        public override string ToString() => "FixtureTokens(redacted)";
    }
}
