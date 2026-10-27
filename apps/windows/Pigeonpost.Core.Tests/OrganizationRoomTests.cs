using System.Net;
using System.Text;
using System.Text.Json;
using Pigeonpost.Core;
using Pigeonpost.Core.Enterprise;

internal static class OrganizationRoomTests
{
    private const string Own = "/k/ec55xq18rbwdnwpsvkht07az40", Other = "/k/rywv63mb6h0hhrpfjewrrttspw", Agent = "/k/cz6900v2h90vnwefj7g7ezvbh4";
    private static void Require(bool value) { if (!value) throw new Exception("Organization room assertion failed."); }
    private static async Task Done(Task? task) { Require(task is not null); await task!.WaitAsync(TimeSpan.FromSeconds(5)); }
    private static RoomOrganization Org(string id = "org_a", string? mailbox = Own, string creation = "everyone", string status = "active", string[]? roles = null) =>
        new(id, "Example " + id, status, "mem_" + id, "example", creation, roles ?? [], mailbox is null ? null : new(mailbox, "/example/me"));
    private static RoomOrganizations Catalog(params RoomOrganization[] orgs) => new(orgs, []);
    private static ConfirmedRoom Confirmed(string id, string org = "org_a", string role = "owner", string state = "active") => new(id, org, "Room " + id, state, role, Own, "active");

    private sealed class Inbox : IInboxService
    {
        public AuthorizationLease Root = new();
        public string? Owner = new('a', 64);
        public IReadOnlyList<Mailbox> Boxes = [new(Own, "/example/me"), new(Other, "/personal/me")];
        public AuthorizationLease? AcceptedAuthorization => Root;
        public string? RecoveryOwnerKey => Owner;
        public string? RecoveryOrigin => "https://rooms-fixture.invalid";
        public Task<IReadOnlyList<Mailbox>> GetMailboxesAsync(CancellationToken ct) => Task.FromResult(Boxes);
        public Task<InboxSnapshot> LoadAsync(string identity, CancellationToken ct) => throw new NotSupportedException();
        public Task<SendReceipt> SendAsync(string identity, string peer, string body, string? thread, CancellationToken ct) => throw new NotSupportedException();
        public Task<string> CreateThreadAsync(string identity, string peer, string title, CancellationToken ct) => throw new NotSupportedException();
        public Task SetArchivedAsync(string identity, string peer, bool archived, CancellationToken ct) => throw new NotSupportedException();
        public Task AcknowledgeAsync(string identity, string message, CancellationToken ct) => throw new NotSupportedException();
        public Task SetContactAsync(string identity, Contact contact, CancellationToken ct) => throw new NotSupportedException();
        public Task RemoveContactAsync(string identity, string peer, CancellationToken ct) => throw new NotSupportedException();
        public Task DeleteThreadAsync(string identity, string thread, CancellationToken ct) => throw new NotSupportedException();
    }
    private sealed class Api(Inbox inbox) : IOrganizationRoomApi
    {
        public AuthorizationLease? AcceptedAuthorization => inbox.Root;
        public RoomOrganizations Orgs = Catalog(Org());
        public List<OrganizationRoom> Rooms = [new("grp_public", "Design", null, 3, "public", null), new("grp_private", "Board", null, 2, "private", "invited")];
        public Func<OrganizationRoomCreate, Task<ConfirmedRoom>>? OnCreate;
        public Func<string, Task<ConfirmedRoom>>? OnRead;
        public Func<Task>? OnRooms;
        public int Creates, Joins, Accepts, Reads, RoomLoads; public readonly List<string> Bodies = [];
        public readonly List<AuthorizationLease?> Actors = [];
        public Task<RoomOrganizations> OrganizationsAsync(string mailbox, CancellationToken ct) { Actors.Add(AuthorizationContext.Current); return Task.FromResult(Orgs); }
        public async Task<IReadOnlyList<OrganizationRoom>> RoomsAsync(string mailbox, string org, string search, CancellationToken ct)
        { RoomLoads++; if (OnRooms is not null) await OnRooms(); return Rooms.Where(r => r.Name.Contains(search, StringComparison.OrdinalIgnoreCase)).ToArray(); }
        public Task<RoomPreview> PreviewRoomAsync(string mailbox, string id, CancellationToken ct)
        { var r = Rooms.Single(x => x.Id == id); return Task.FromResult(new RoomPreview(r.Id, r.Name, r.Description, r.MemberCount, r.JoinPolicy, r.Membership, r.JoinPolicy == "public")); }
        public Task<ConfirmedRoom> RoomAsync(string mailbox, string id, CancellationToken ct)
        { Reads++; return OnRead?.Invoke(id) ?? Task.FromResult(Confirmed(id, role: "member", state: "keying")); }
        public Task<ConfirmedRoom> CreateRoomAsync(string mailbox, OrganizationRoomCreate payload, CancellationToken ct)
        { Creates++; Bodies.Add(payload.Encoded); return OnCreate?.Invoke(payload) ?? Task.FromResult(Confirmed("grp_created")); }
        public Task JoinRoomAsync(string mailbox, string id, CancellationToken ct) { Joins++; return Task.CompletedTask; }
        public Task AcceptRoomInvitationAsync(string mailbox, string id, CancellationToken ct) { Accepts++; return Task.CompletedTask; }
    }
    private sealed class World
    {
        public readonly Inbox Inbox = new(); public readonly Api Api; public IRoomCreationStore Custody = new MemoryRoomCreationStore();
        public string Mailbox = Own;
        public World() => Api = new(Inbox);
        public OrganizationRoomStore Store()
        {
            var store = new OrganizationRoomStore(Api, Inbox, Custody, () => Inbox.Owner is { } owner ? new(owner, Inbox.Root, Mailbox) : null);
            store.Synchronize(); return store;
        }
    }

    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Organization membership is canonical: handles, delegation and other inboxes grant nothing", () =>
        {
            var catalog = new RoomOrganizations([Org("org_a"), Org("org_b", mailbox: Other), Org("org_c", mailbox: null)],
                [new(Agent, "org_c", "/example/agent", ["read_inbox"]), new(Own, "org_c", "/example/me", ["enroll"])]);
            Require(catalog.Valid && catalog.OwnedFor(Own).Single().Id == "org_a" && catalog.OwnedFor(Agent).Count == 0);
            Require(catalog.Delegated.Single().Address == Agent && catalog.OwnedFor("/example/me").Count == 0);
            Require(Org().MayCreate(Own) && !Org().MayCreate(Other) && !Org(creation: "admins").MayCreate(Own)
                && Org(creation: "admins", roles: ["people_admin"]).MayCreate(Own) && !Org(status: "read_only").MayCreate(Own) && Org(status: "read_only").Readable);
            Require(!new RoomOrganizations([Org(), Org()], []).Valid && !new RoomOrganizations([Org()], [new(Agent, "org_missing", null, ["read_inbox"])]).Valid);
            Require(!Org(roles: ["owner", "owner"]).Valid && !(Org() with { Name = "visible‮override" }).Valid);
            Require(new OrganizationRoom("grp", "Room", null, 1, "public", "left").Action == RoomAction.Join
                && new OrganizationRoom("grp", "Room", null, 1, "private", null).Action == RoomAction.Unavailable
                && new OrganizationRoom("grp", "Room", null, 1, "public", "banned").Action == RoomAction.Unavailable);
            Require(OrganizationRoomInput.Text("ğ😀", 2) && !OrganizationRoomInput.Text("a\ud800b", 10) && !OrganizationRoomInput.Text("line\nbreak", 20)
                && OrganizationRoomInput.Text("line\nbreak", 20, multiline: true));
            return Task.CompletedTask;
        });
        yield return ("Room creation bytes are exact and restore only from the identical request", () =>
        {
            var create = new OrganizationRoomCreate("org_a", "  Design ğ ", " Notes ", RoomJoinPolicy.Invited, RoomPostPolicy.Admins, false, "rk_fixture_123456");
            Require(create.Encoded == "{\"request_key\":\"rk_fixture_123456\",\"org_id\":\"org_a\",\"name\":\"Design ğ\",\"description\":\"Notes\",\"join_policy\":\"private\",\"post_policy\":\"admins\",\"visibility\":\"unlisted\",\"history_visible_to_new_members\":false}"
                || create.Encoded.Contains("\\u011F"));
            Require(OrganizationRoomCreate.Restore(create.Encoded).Encoded == create.Encoded && !create.ToString().Contains("Design"));
            foreach (var changed in new[] { create.Encoded.Replace(",", ", "), create.Encoded.Replace("{", "{\"extra\":1,"),
                create.Encoded.Replace("\"name\"", "\"name\":\"x\",\"name\""), create.Encoded.Replace("false}", "true}") })
            {
                try { OrganizationRoomCreate.Restore(changed); throw new Exception("Altered creation restored."); }
                catch (Exception error) when (error is ArgumentException or JsonException) { }
            }
            foreach (var invalid in new Func<OrganizationRoomCreate>[] { () => new("org a", "x", "", RoomJoinPolicy.Members, RoomPostPolicy.Everyone, true, "rk_fixture_123456"),
                () => new("org_a", " ", "", RoomJoinPolicy.Members, RoomPostPolicy.Everyone, true, "rk_fixture_123456"),
                () => new("org_a", new string('a', 81), "", RoomJoinPolicy.Members, RoomPostPolicy.Everyone, true, "rk_fixture_123456"),
                () => new("org_a", "x", "", RoomJoinPolicy.Members, RoomPostPolicy.Everyone, true, "short") })
            { try { invalid(); throw new Exception("Invalid creation accepted."); } catch (ArgumentException) { } }
            return Task.CompletedTask;
        });
        yield return ("Room HTTP requires an organization, decodes strictly and keeps exact creation bytes", async () =>
        {
            var requests = new List<(string Method, string Path, string? Body)>(); var reply = "";
            using var http = new HttpClient(new Handler(async (request, ct) =>
            {
                requests.Add((request.Method.Method, request.RequestUri!.PathAndQuery, request.Content is null ? null : await request.Content.ReadAsStringAsync(ct)));
                Require(request.Headers.GetValues("x-pigeonpost-identity").Single() == Own);
                return new HttpResponseMessage(HttpStatusCode.OK) { Content = new StringContent(reply, Encoding.UTF8, "application/json") };
            }));
            var tokens = new Tokens(); using var api = new PostboxClient(http, tokens, new Uri("http://127.0.0.1:1/"));
            reply = "{\"orgs\":[{\"id\":\"org_a\",\"name\":\"Example\",\"status\":\"active\",\"my_member_id\":\"mem_1\",\"my_roles\":[\"member\"],\"settings\":{\"rooms_creation\":\"everyone\"},\"my_mailbox\":{\"address\":\"" + Own + "\",\"handle\":\"/example/me\"}}],\"operated\":[]}";
            var catalog = await api.OrganizationsAsync(Own, default); Require(catalog.OwnedFor(Own).Single().MayCreate(Own));
            foreach (var bad in new[] { "{\"orgs\":[],\"orgs\":[]}", "{\"orgs\":[{\"id\":\"org_a\",\"name\":\"x\",\"status\":\"active\"}]}", "[]", "{\"orgs\":\"x\"}" })
            { reply = bad; try { await api.OrganizationsAsync(Own, default); throw new Exception("Bad catalog accepted."); } catch (PostboxException e) when (e.Code == "bad_response") { } }
            try { await api.RoomsAsync(Own, "", "", default); throw new Exception("Missing organization accepted."); } catch (ArgumentException) { }
            try { await api.RoomsAsync(Own, "org_a", new string('a', 81), default); throw new Exception("Long search accepted."); } catch (ArgumentException) { }
            reply = "{\"rooms\":[{\"id\":\"grp_a\",\"name\":\"Design\",\"member_count\":2,\"join_policy\":\"public\"}]}";
            Require((await api.RoomsAsync(Own, "org_a", "de sign", default)).Single().Action == RoomAction.Join && requests[^1].Path.StartsWith("/v1/rooms?org_id=org_a&q=de%20sign&identity=", StringComparison.Ordinal));
            var group = "{\"id\":\"grp_a\",\"org_id\":\"org_a\",\"name\":\"Design\",\"created_by\":\"" + Own + "\",\"my\":{\"role\":\"owner\",\"state\":\"active\"}}";
            reply = group; Require((await api.RoomAsync(Own, "grp_a", default)).Role == "owner");
            reply = "{\"group\":" + group + "}"; Require((await api.RoomAsync(Own, "grp_a", default)).OrganizationId == "org_a");
            foreach (var bad in new[] { group.Replace(",\"org_id\":\"org_a\"", ""), group.Replace("\"state\":\"active\"", "\"state\":\"invited\""), group.Replace("grp_a", "grp_b"),
                group.Replace("{\"role\":\"owner\",", "{\"role\":\"owner\",\"role\":\"admin\","), group.Replace("\"my\":{\"role\":\"owner\",\"state\":\"active\"}", "\"my\":null") })
            { reply = bad; try { await api.RoomAsync(Own, "grp_a", default); throw new Exception("Unconfirmed room accepted."); } catch (PostboxException e) when (e.Code == "bad_response") { } }
            var payload = new OrganizationRoomCreate("org_a", "Design", "", RoomJoinPolicy.Members, RoomPostPolicy.Everyone, true, "rk_fixture_123456");
            reply = "{\"group\":" + group + "}"; Require((await api.CreateRoomAsync(Own, payload, default)).Id == "grp_a" && requests[^1].Body == payload.Encoded);
            reply = "{\"group\":" + group.Replace("\"owner\"", "\"member\"") + "}";
            try { await api.CreateRoomAsync(Own, payload, default); throw new Exception("Creation without ownership accepted."); } catch (PostboxException e) when (e.Code == "bad_response") { }
            reply = "{\"id\":\"grp_a\",\"name\":\"Design\",\"member_count\":2,\"join_policy\":\"public\",\"can_join\":true}";
            Require((await api.PreviewRoomAsync(Own, "grp_a", default)).CanJoin && requests[^1].Body == "{\"group_id\":\"grp_a\"}");
            reply = ""; await api.JoinRoomAsync(Own, "grp_a", default); await api.AcceptRoomInvitationAsync(Own, "grp_a", default);
            Require(requests[^2].Path.StartsWith("/v1/groups/grp_a/join", StringComparison.Ordinal) && requests[^1].Body == "{\"accept\":true}");
            try { await api.JoinRoomAsync(Own, "../x", default); throw new Exception("Substituted path accepted."); } catch (ArgumentException) { }
            using (new AuthorizationContext(tokens.AcceptedAuthorization!))
            {
                tokens.Replace(); var sent = requests.Count;
                try { await api.OrganizationsAsync(Own, default); throw new Exception("Retired account read rooms."); } catch (AuthorizationRetiredException) { }
                Require(requests.Count == sent);
            }
        });
        yield return ("Room store binds actor, organization and reviewed screen before opening a public join or invitation", async () =>
        {
            var w = new World(); var store = w.Store(); var context = store.State.ContextId;
            await Done(store.LoadOrganizations());
            Require(store.State.Organization?.Id == "org_a" && store.State.Rooms.Count == 2 && store.State.CustodyChecked && w.Api.Actors.Single()!.SharesAuthorization(w.Inbox.Root));
            Require(store.Inspect("grp_public", "stale", context) is null);
            await Done(store.Inspect("grp_public", store.State.DirectoryProof, context));
            var proof = store.State.ReviewProof; Require(store.ReviewAction(proof, context) == RoomAction.Join);
            Require(store.OpenReviewed("stale", context) is null && w.Api.Joins == 0);
            await Done(store.OpenReviewed(proof, context));
            Require(w.Api.Joins == 1 && w.Api.Reads == 1 && store.State.Opening is { GroupId: "grp_public", OrganizationId: "org_a" } opening && store.Allows(opening));
            store.OpeningCompleted(store.State.Opening!); Require(store.State.Opening is null);
            await Done(store.Inspect("grp_private", store.State.DirectoryProof, context));
            Require(store.ReviewAction(store.State.ReviewProof, context) == RoomAction.AcceptInvitation);
            w.Api.OnRead = id => Task.FromResult(Confirmed(id, org: "org_other", role: "member"));
            await Done(store.OpenReviewed(store.State.ReviewProof, context));
            Require(w.Api.Accepts == 1 && store.State.Opening is null && store.State.MustReviewAgain && store.ReviewAction(store.State.ReviewProof, context) == RoomAction.Unavailable);
        });
        yield return ("Room store discards delayed work after account or inbox replacement and refusal clears metadata", async () =>
        {
            var w = new World(); var store = w.Store(); await Done(store.LoadOrganizations());
            var hold = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously); w.Api.OnRooms = () => hold.Task;
            var pending = store.LoadRooms("De"); Require(store.State.LoadingRooms);
            w.Mailbox = Other; store.Synchronize(); Require(store.State.Rooms.Count == 0 && store.State.Catalog is null && store.State.Mailbox == Other);
            hold.SetResult(); await Done(pending); Require(store.State.Rooms.Count == 0 && !store.State.LoadingRooms);
            await Done(store.LoadOrganizations()); Require(store.State.Organizations.Count == 0 && store.State.OtherInboxes.Single().Id == "org_a" && !store.CanCreate(store.State.ContextId));
            w.Mailbox = Own; w.Api.OnRooms = null; store.Synchronize(); await Done(store.LoadOrganizations());
            w.Api.OnRooms = () => throw new PostboxException(403, "forbidden");
            await Done(store.LoadRooms(""));
            Require(store.State.Catalog is null && store.State.Rooms.Count == 0 && store.State.Message!.Contains("Access could not be confirmed"));
            var before = store.State.ContextId; var old = w.Inbox.Root; w.Inbox.Root = new(); old.Retire(); store.Synchronize();
            Require(store.State.ContextId != before && !store.IsCurrent(before) && store.State.Catalog is null && store.Inspect("grp_public", store.State.DirectoryProof, before) is null);
        });
        yield return ("A lost room creation stays in custody, restores after restart and retries the exact request only by hand", async () =>
        {
            using var protector = new MessageAttemptStoreTests.Protector();
            var folder = Path.Combine(Path.GetTempPath(), "pp-room-custody-" + Guid.NewGuid().ToString("N"));
            try
            {
                var w = new World { Custody = new FileRoomCreationStore(folder, protector) }; var store = w.Store(); await Done(store.LoadOrganizations());
                var context = store.State.ContextId; Require(store.BeginCreation(context));
                store.ChangeDraft(store.State.Draft! with { Name = "Private plans ğ", Description = "Synthetic" }, context);
                w.Api.OnCreate = _ => throw new HttpRequestException("synthetic committed loss");
                await Done(store.SubmitDraft(store.State.Draft!.Id, context));
                Require(w.Api.Creates == 1 && store.State.PendingCreation is { CandidateId: null } && store.State.Message!.Contains("not confirmed"));
                var files = Directory.GetFiles(folder, "*", SearchOption.AllDirectories);
                Require(files.Length > 0 && files.All(f => !Encoding.UTF8.GetString(File.ReadAllBytes(f)).Contains("Private plans", StringComparison.Ordinal)));
                store.ChangeDraft(store.State.Draft! with { Name = "Changed" }, context); Require(store.State.Draft!.Name == "Private plans ğ");
                store.Dispose();
                var restarted = w.Store(); await Done(restarted.LoadOrganizations());
                Require(restarted.State.PendingCreation is { } restored && restored.Payload.Encoded == w.Api.Bodies[0] && w.Api.Creates == 1);
                Require(restarted.BeginCreation(restarted.State.ContextId) && restarted.State.Draft!.Name == "Private plans ğ");
                w.Api.OnCreate = null;
                await Done(restarted.SubmitDraft(restarted.State.Draft!.Id, restarted.State.ContextId));
                Require(w.Api.Creates == 2 && w.Api.Bodies.Distinct().Count() == 1 && restarted.State.CompletedCreation?.Id == "grp_created" && restarted.State.PendingCreation is null);
                Require(restarted.State.Opening?.GroupId == "grp_created" && Directory.GetFiles(folder, "room.pending", SearchOption.AllDirectories).Length == 0);
                Require(restarted.SubmitDraft("any", restarted.State.ContextId) is null && w.Api.Creates == 2);
            }
            finally { if (Directory.Exists(folder)) Directory.Delete(folder, recursive: true); }
        });
        yield return ("A known created room is checked by reading it, and discard removes only local custody", async () =>
        {
            var w = new World(); var store = w.Store(); await Done(store.LoadOrganizations()); var context = store.State.ContextId;
            store.BeginCreation(context); store.ChangeDraft(store.State.Draft! with { Name = "Ops" }, context);
            w.Api.OnRead = _ => throw new HttpRequestException("synthetic read loss");
            await Done(store.SubmitDraft(store.State.Draft!.Id, context));
            Require(w.Api.Creates == 1 && store.State.PendingCreation?.CandidateId == "grp_created" && store.SubmitDraft(store.State.Draft!.Id, context) is null);
            Require(w.Api.Creates == 1);
            w.Api.OnRead = null; await Done(store.CheckCreated(context));
            Require(w.Api.Creates == 1 && store.State.CompletedCreation?.Id == "grp_created" && store.State.PendingCreation is null);
            var second = w.Store(); await Done(second.LoadOrganizations()); var c2 = second.State.ContextId;
            second.BeginCreation(c2); second.ChangeDraft(second.State.Draft! with { Name = "Lost" }, c2);
            w.Api.OnCreate = _ => throw new HttpRequestException("loss");
            await Done(second.SubmitDraft(second.State.Draft!.Id, c2)); Require(second.State.PendingCreation is not null && second.Close());
            var third = w.Store(); await Done(third.LoadOrganizations()); Require(third.State.PendingCreation is not null);
            await Done(third.DiscardCreation(third.State.ContextId));
            Require(third.State.PendingCreation is null && w.Api.Creates == 2);
            var fourth = w.Store(); await Done(fourth.LoadOrganizations()); Require(fourth.State.PendingCreation is null);
        });
        yield return ("Creation is disabled without protected custody, for read-only organizations and for other inboxes", async () =>
        {
            var w = new World(); w.Inbox.Owner = null; var store = w.Store(); Require(!store.IsCurrent() && store.LoadOrganizations() is null);
            w.Inbox.Owner = "not-a-protected-owner-key"; store.Synchronize(); await Done(store.LoadOrganizations());
            Require(store.State.Organization is not null && !store.State.CustodyChecked && !store.CanCreate(store.State.ContextId) && !store.BeginCreation(store.State.ContextId));
            var unlisted = new World(); unlisted.Inbox.Boxes = [new(Other)]; var refused = unlisted.Store(); await Done(refused.LoadOrganizations());
            Require(refused.State.Catalog is null && !refused.CanCreate(refused.State.ContextId));
            var r = new World(); r.Api.Orgs = Catalog(Org(status: "read_only")); var readOnly = r.Store(); await Done(readOnly.LoadOrganizations());
            Require(readOnly.State.Organization is { Readable: true, Writable: false } && !readOnly.CanCreate(readOnly.State.ContextId));
            await Done(readOnly.Inspect("grp_public", readOnly.State.DirectoryProof, readOnly.State.ContextId));
            Require(readOnly.ReviewAction(readOnly.State.ReviewProof, readOnly.State.ContextId) == RoomAction.Unavailable);
        });
    }
    public static IEnumerable<(string Name, Func<Task> Run)> StandIn()
    {
        yield return ("Room openings activate owned rooms with real MLS and refuse rooms after offboarding", async () =>
        {
            var server = new Pigeonpost.Core.Preview.PreviewPostbox(); server.Features.Add(ServerFeatures.Organizations);
            var me = server.AddMailbox(Pigeonpost.Core.Preview.PreviewPostbox.Seed(91), "/example/me");
            var owner = server.AddMailbox(Pigeonpost.Core.Preview.PreviewPostbox.Seed(92), "/example/owner");
            server.AddOrganization("org_example", "Example", [me, owner], [("/k/cz6900v2h90vnwefj7g7ezvbh4", "/example/agent")]);
            var seeded = server.SeedOrganizationRoom("org_example", owner, "Design", "public", "Product design");
            var tokens = new Tokens(); var mailbox = new Mailbox(me, "/example/me");
            using var client = new PostboxClient(new HttpClient(server), tokens, new Uri("http://127.0.0.1:9"));
            var inbox = new StandInInbox(client, tokens, [mailbox]);
            var store = new OrganizationRoomStore(client, inbox, new MemoryRoomCreationStore(), () => new(new string('d', 64), tokens.AcceptedAuthorization!, me));
            using var groups = new Pigeonpost.Core.Groups.GroupsModel(client, _ => new Pigeonpost.Core.Groups.MemoryGroupStateStore());
            await groups.ActivateAsync(mailbox, await client.GetCapabilitiesAsync(default), [mailbox], []);
            store.Synchronize(); await Done(store.LoadOrganizations());
            Require(store.State.Organization?.Id == "org_example" && store.State.Delegated.Single().Handle == "/example/agent" && store.State.Rooms.Single().Id == seeded);
            await Done(store.Inspect(seeded, store.State.DirectoryProof, store.State.ContextId));
            await Done(store.OpenReviewed(store.State.ReviewProof, store.State.ContextId));
            var joined = store.State.Opening!; Require(joined.GroupId == seeded);
            Require(await groups.PrepareOrganizationRoomAsync(joined.GroupId, joined.OrganizationId) && groups.Selected?.Id == seeded && groups.ShowsKeying);
            store.OpeningCompleted(joined);
            Require(store.BeginCreation(store.State.ContextId));
            store.ChangeDraft(store.State.Draft! with { Name = "Launch" }, store.State.ContextId);
            await Done(store.SubmitDraft(store.State.Draft!.Id, store.State.ContextId));
            var created = store.State.Opening!; Require(store.State.CompletedCreation?.Role == "owner");
            Require(await groups.PrepareOrganizationRoomAsync(created.GroupId, created.OrganizationId) && groups.Selected?.Id == created.GroupId && groups.ShowsComposer);
            groups.Draft = "Hello room"; Require(await groups.SendAsync());
            try { await groups.PrepareOrganizationRoomAsync(created.GroupId, "org_other"); throw new Exception("Another organization opened the room."); }
            catch (PostboxException error) when (error.Code == "not_member") { }
            server.RemoveOrganizationMember("org_example", me);
            try { await groups.PrepareOrganizationRoomAsync(created.GroupId, created.OrganizationId); throw new Exception("Offboarded member opened the room."); }
            catch (PostboxException error) when (error.StatusCode == 403) { }
            await Done(store.LoadRooms("")); Require(store.State.Catalog is null);
        });
    }
    private sealed class StandInInbox(PostboxClient client, Tokens tokens, IReadOnlyList<Mailbox> boxes) : IInboxService
    {
        public AuthorizationLease? AcceptedAuthorization => tokens.AcceptedAuthorization;
        public string? RecoveryOwnerKey => new string('d', 64);
        public string? RecoveryOrigin => "https://rooms-stand-in.invalid";
        public Task<IReadOnlyList<Mailbox>> GetMailboxesAsync(CancellationToken ct) => Task.FromResult(boxes);
        public Task<InboxSnapshot> LoadAsync(string identity, CancellationToken ct) => client.LoadAsync(identity, ct);
        public Task<SendReceipt> SendAsync(string identity, string peer, string body, string? thread, CancellationToken ct) => throw new NotSupportedException();
        public Task<string> CreateThreadAsync(string identity, string peer, string title, CancellationToken ct) => throw new NotSupportedException();
        public Task SetArchivedAsync(string identity, string peer, bool archived, CancellationToken ct) => throw new NotSupportedException();
        public Task AcknowledgeAsync(string identity, string message, CancellationToken ct) => throw new NotSupportedException();
        public Task SetContactAsync(string identity, Contact contact, CancellationToken ct) => throw new NotSupportedException();
        public Task RemoveContactAsync(string identity, string peer, CancellationToken ct) => throw new NotSupportedException();
        public Task DeleteThreadAsync(string identity, string thread, CancellationToken ct) => throw new NotSupportedException();
    }
    private sealed class Handler(Func<HttpRequestMessage, CancellationToken, Task<HttpResponseMessage>> send) : HttpMessageHandler
    { protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken ct) => send(request, ct); }
    private sealed class Tokens : IAccessTokenProvider
    {
        public AuthorizationLease? AcceptedAuthorization { get; private set; } = new();
        public Task<string> GetTokenAsync(CancellationToken ct) => Task.FromResult("fixture-rooms");
        public Task<string> RefreshTokenAsync(CancellationToken ct) => throw new Exception("Unexpected renewal");
        public void Replace() { var old = AcceptedAuthorization; AcceptedAuthorization = new(); old?.Retire(); }
    }
}
