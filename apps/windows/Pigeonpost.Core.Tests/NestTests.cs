using System.Net;
using System.Text;
using System.Text.Json;
using Pigeonpost.Core;
using Pigeonpost.Core.Nests;

// Nests on Windows (docs/nests/phase-b.md): the rules behind the screens and the wire contract with
// the postbox routes in crates/pigeonpost-postbox/src/nest_*.rs.
internal static class NestTests
{
    private static readonly string Hash = new('a', 64);
    private const string NestId = "nst_0123456789abcdef";
    private static string Emoji(int count) => string.Concat(Enumerable.Repeat("\U0001F426", count));
    private static JsonElement Json(string text) { using var doc = JsonDocument.Parse(text); return doc.RootElement.Clone(); }
    private static NestRow Row(string status, string? reason = null, bool owner = true, string? address = "/k/nest1") =>
        new(NestId, address, "Helper", status == "paused" ? "paused" : "running", status, reason, owner);
    private static NestApproval Card(string status = "pending") => new("nap_0123456789abcdef01234567", "send_message", Hash, status, "/bob", "hello");

    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Nests stay hidden unless the postbox reports the feature", () =>
        {
            Require(!NestRules.IsOn(ServerFeatures.None), "off by default");
            Require(NestRules.IsOn(new ServerFeatures(new HashSet<string>(StringComparer.Ordinal) { "nests" })), "on when listed");
            return Task.CompletedTask;
        });
        yield return ("Nest names are measured the way the server measures them", () =>
        {
            Require(NestRules.ValidLabel("Helper") && NestRules.ValidLabel("  Helper  "), "ordinary names");
            Require(NestRules.ValidLabel(Emoji(64)), "64 emoji are 64 characters, not 128");
            Require(!NestRules.ValidLabel(Emoji(65)) && !NestRules.ValidLabel(new string('x', 65)), "too long");
            Require(!NestRules.ValidLabel("   ") && !NestRules.ValidLabel(null) && !NestRules.ValidLabel("a\u0001b"), "blank or control characters");
            return Task.CompletedTask;
        });
        yield return ("Only the exact name deletes a Nest", () =>
        {
            Require(NestRules.DeleteConfirmed("Helper", "Helper"), "exact");
            Require(!NestRules.DeleteConfirmed("Helper", "helper") && !NestRules.DeleteConfirmed("Helper", "Helper ") && !NestRules.DeleteConfirmed("Helper", null), "near misses");
            return Task.CompletedTask;
        });
        yield return ("Upload names become percent-encoded UTF-8 without control characters", () =>
        {
            Require(NestRules.UploadHeader("a b.pdf") == "a%20b.pdf", "space");
            Require(NestRules.UploadHeader("résumé.txt") == "r%C3%A9sum%C3%A9.txt", "UTF-8");
            Require(NestRules.UploadHeader("a\nb\u0000c.txt") == "abc.txt", "controls removed");
            var header = NestRules.UploadHeader(Emoji(250));
            Require(Uri.UnescapeDataString(header) == Emoji(200), "a long name is cut between characters");
            Require(!header.Contains("%EF%BF%BD"), "never half a surrogate pair");
            try { NestRules.UploadHeader("\n\n"); throw new InvalidOperationException("A nameless file was accepted."); }
            catch (ArgumentException) { }
            return Task.CompletedTask;
        });
        yield return ("Pause and Resume follow the server's own rules", () =>
        {
            Require(NestRules.CanPause(Row("ready")) && NestRules.CanPause(Row("working")), "running");
            Require(!NestRules.CanPause(Row("paused")) && !NestRules.CanPause(Row("unavailable", "setting_up")) && !NestRules.CanPause(Row("ready", owner: false)), "not running or not an admin");
            Require(NestRules.CanResume(Row("paused")), "owner's own pause");
            Require(!NestRules.CanResume(Row("paused", "over_capacity")) && !NestRules.CanResume(Row("needs_you", "payment")) && !NestRules.CanResume(Row("paused", owner: false)), "funding pauses and operators");
            return Task.CompletedTask;
        });
        yield return ("A conversation finds its Nest by address or by its handle", () =>
        {
            var snapshot = new NestSnapshot(1, 1, "nest.ai.starter.monthly", 0, [Row("ready", address: "/k/nest1")], []);
            Require(NestRules.ForPeer(snapshot, "/k/nest1", a => a)?.Id == NestId, "same address");
            Require(NestRules.ForPeer(snapshot, "/acme/hr", a => a == "/k/nest1" ? "/acme/hr" : a)?.Id == NestId, "known by handle");
            Require(NestRules.ForPeer(snapshot, "/someone/else", a => a) is null && NestRules.ForPeer(null, "/k/nest1", a => a) is null && NestRules.ForPeer(snapshot, null, a => a) is null, "others have no status");
            return Task.CompletedTask;
        });
        yield return ("Status words and reasons match the header states", () =>
        {
            Require(NestRules.StatusLine(Row("ready")) == ("Ready", null), "ready");
            Require(NestRules.StatusLine(Row("working")).Text == "Working…", "working");
            Require(NestRules.StatusLine(Row("needs_you", "payment")) is ("Needs you", { } why) && why.Contains("Renew"), "payment");
            Require(NestRules.StatusLine(Row("paused", "over_capacity")).Reason!.Contains("fewer Nests"), "over capacity");
            Require(NestRules.StatusLine(Row("unavailable", "setting_up")) == ("Unavailable", "Setting up…"), "setting up");
            return Task.CompletedTask;
        });
        yield return ("The packages wire keeps only what is on sale", () =>
        {
            var packages = NestRules.Packages(Json("""
                {"catalog_version":"nests-2026-11","packages":[
                {"code":"nest.ai.starter.monthly","name":"Pigeonpost AI","slots":1,"storage_bytes":5368709120,"active_hours":10,"input_tokens":1000000,"output_tokens":100000,"model":"managed","sellable":true},
                {"code":"nest.ai.pro.monthly","name":"Pigeonpost AI Pro","slots":10,"storage_bytes":107374182400,"active_hours":120,"input_tokens":15000000,"output_tokens":1500000,"model":"managed","sellable":false},
                {"code":"nest.host.starter.monthly","name":"Empty Nest","slots":1,"storage_bytes":5368709120,"active_hours":10,"input_tokens":0,"output_tokens":0,"model":"none","sellable":true}]}
                """));
            Require(packages.Select(p => p.Code).SequenceEqual(["nest.ai.starter.monthly", "nest.host.starter.monthly"]), "sellable only");
            var details = NestRules.PackageDetails(packages[0]);
            Require(details.StartsWith("1 Nest · 5 GiB files · 10 hours a month") && details.Contains("AI included"), details);
            Require(NestRules.PackageDetails(packages[1]).Contains("No AI included"), "no model");
            return Task.CompletedTask;
        });
        yield return ("The list wire reads the allowance, setups and rows, and survives wide names", () =>
        {
            var snapshot = NestRules.Snapshot(Json($$"""
                {"allowance":{"slots":3,"used":1,"funding":{"package":"nest.ai.plus.monthly","status":"active"},"duplicates":[{"package":"x"}]},
                 "nests":[{"nest_id":"{{NestId}}","address":"/k/nest1","label":"{{Emoji(64)}}","state":"running","status":"mystery","status_reason":null}],
                 "setups":[{"setup_id":"nsu_0123456789ab","label":"First Nest","status":"failed","last_error":"{{new string('e', 900)}}"},{"setup_id":"bad","label":"x","status":"pending"}]}
                """));
            Require(snapshot is { Slots: 3, Used: 1, Package: "nest.ai.plus.monthly", Duplicates: 1 }, "allowance");
            Require(snapshot.Nests.Single().Label == Emoji(64), "a 64-emoji name is legal");
            Require(snapshot.Nests.Single() is { Status: "unavailable", IsOwner: true }, "unknown status is never shown as ready; own rows carry no role");
            var setup = snapshot.Setups.Single();
            Require(setup.Failed && setup.LastError!.Length <= 201, "bad ids dropped and long errors shortened, not fatal");
            Require(NestRules.SetupLine(setup) == "First Nest: paid, setting up failed", "failed setup line");
            Require(NestRules.AllowanceLine(snapshot, [new("nest.ai.plus.monthly", "AI Plus", 3, 1, 1, 1, 1, true)]) == "Your package: AI Plus. 1 of 3 Nests in use.", "allowance line");
            return Task.CompletedTask;
        });
        yield return ("Unexpected Nest answers are refused rather than shown", () =>
        {
            foreach (var bad in new[] { $$"""{"nest_id":"nst_../x","label":"a"}""", $$"""{"nest_id":"{{NestId}}"}""", $$"""{"nest_id":"{{NestId}}","label":7}""", $$"""{"nest_id":"{{NestId}}","label":"{{new string('x', 65)}}"}""" })
            {
                try { NestRules.Nest(Json(bad)); throw new InvalidOperationException("Accepted: " + bad); }
                catch (NestWireException) { }
            }
            return Task.CompletedTask;
        });
        yield return ("The files wire reports coverage and keeps explanations displayable", () =>
        {
            var files = NestRules.Files(Json($$"""
                {"used_bytes":1572864,"quota_bytes":5368709120,"files":[
                {"file_id":"nfl_0123456789abcdef01234567","name":"plan.pdf","bytes":10,"status":"partial","reason":null,"coverage":{"units_total":4,"units_text":3},"warnings":["w\u0001arn",5]},
                {"file_id":"nfl_0123456789abcdef01234568","name":"bad.bin","bytes":10,"status":"rejected","reason":"{{new string('r', 700)}}","coverage":{"units_total":null,"units_text":null},"warnings":[]},
                {"file_id":"nfl_0123456789abcdef01234569","name":"note.md","bytes":10,"status":"parsed","reason":null,"coverage":{"units_total":0,"units_text":0},"warnings":[]}]}
                """));
            Require(NestRules.StorageLine(files) == "1.5 MiB of 5 GiB used", NestRules.StorageLine(files));
            Require(NestRules.CoverageText(files.Files[0]) == "Read 3 of 4" && files.Files[0].Warnings.Single() == "warn", "coverage and clean warnings");
            Require(NestRules.CoverageText(files.Files[1]).Length <= 301, "a long reason is shortened");
            Require(NestRules.CoverageText(files.Files[2]) == "Read", "plain parse");
            return Task.CompletedTask;
        });
        yield return ("Approval cards show the real call and only pending ones are decided", () =>
        {
            var cards = NestRules.Approvals(Json($$$"""
                {"approvals":[
                {"approval_id":"nap_0123456789abcdef01234567","tool":"send_message","call":{"tool":"send_message","args":{"body":"line one\nline two","to":"/bob"}},"args_hash":"{{{Hash}}}","status":"pending"},
                {"approval_id":"nap_0123456789abcdef01234568","tool":"future_tool","call":null,"args_hash":"{{{Hash}}}","status":"approved"}]}
                """));
            Require(NestRules.CardText(cards[0]) == "Send a message to /bob:\nline one\nline two", NestRules.CardText(cards[0]));
            Require(NestRules.CardText(cards[1]) == "future_tool", "other tools are named");
            Require(NestRules.Pending(cards).Single().Id == cards[0].Id, "pending only");
            try { NestRules.Approvals(Json($$"""{"approvals":[{"approval_id":"nap_x","tool":"t","args_hash":"{{Hash}}"}]}""")); throw new InvalidOperationException("Bad id accepted."); }
            catch (NestWireException) { }
            return Task.CompletedTask;
        });
        yield return ("Usage and organization answers read as the server sends them", () =>
        {
            var lines = NestRules.UsageLines(Json("""{"input_tokens":{"used":1500,"limit":1000000},"output_tokens":{"used":20,"limit":100000},"running_secs":3720,"files_bytes":1048576}"""));
            Require(lines[0].Contains("1,500") && lines[0].Contains("1,000,000") && lines[1] == "Running time this month: 1 h 2 min" && lines[2] == "Files: 1.0 MiB", string.Join("|", lines));
            var org = NestRules.OrgNestsOf(Json($$"""{"nests":[{"nest_id":"{{NestId}}","address":"/k/o","label":"HR","state":"running","role":"operator"}],"allowance":{"slots":2,"used":1},"package":null}"""));
            Require(org.Nests.Single() is { Label: "HR", IsOwner: false } && NestRules.RoleLabel(org.Nests[0]) == "operator", "operator row");
            Require(NestRules.OrgLine(org) == "1 of 2 organization Nests in use.", NestRules.OrgLine(org));
            return Task.CompletedTask;
        });
        yield return ("Snapshots compare by value so an unchanged page is not redrawn", () =>
        {
            NestSnapshot Make(string status) => new(1, 1, "p", 0, [Row(status)], [new("nsu_01", "a", false, null)]);
            Require(NestRules.SameSnapshot(Make("ready"), Make("ready")) && !NestRules.SameSnapshot(Make("ready"), Make("working")) && !NestRules.SameSnapshot(null, Make("ready")), "snapshots");
            Require(NestRules.SameCards([Card()], [Card()]) && !NestRules.SameCards([Card()], [Card("approved")]), "cards");
            return Task.CompletedTask;
        });
        yield return ("Every refusal the screens can meet has a plain explanation", () =>
        {
            foreach (var code in new[] { "feature_disabled", "no_nest_package", "nest_slots_full", "approval_changed", "approval_closed", "recipient_not_allowed", "needs_payment", "over_capacity", "storage_full", "too_large", "confirm_required", "export_too_large", "no_nest_slot", "bad_label", "bad_name", "too_many_files", "too_many_pending", "rate_limited", "no_nest", "no_approval", "no_file", "no_setup", "org_inactive", "tool_failed", "nests_unavailable", "nestd_failed", "human_required", "credential_not_accepted" })
                Require(NestRules.Explain(new PostboxException(400, code)) != "Pigeonpost could not complete this. Try again.", "unexplained: " + code);
            Require(NestRules.Explain((Exception)new HttpRequestException()).StartsWith("Could not connect"), "offline");
            Require(NestRules.EndsAccess(new PostboxException(404, "feature_disabled")) && NestRules.EndsAccess(new PostboxException(403, "x")) && !NestRules.EndsAccess(new PostboxException(409, "approval_changed")), "only access refusals clear the page");
            return Task.CompletedTask;
        });
        yield return ("Export suggestions never carry path characters", () =>
        {
            Require(NestRules.ExportFileName("My Nest") == "My Nest.zip", "plain");
            Require(NestRules.ExportFileName("..\\a/b:c") == "abc.zip", "separators");
            Require(NestRules.ExportFileName("///") == "Nest export.zip", "nothing left");
            return Task.CompletedTask;
        });
        yield return ("A decision echoes the hash on the card and the routes are the server's", async () =>
        {
            var server = new Postbox();
            using var client = server.Client();
            await client.DecideNestApprovalAsync(NestId, Card(), approve: true, default);
            await client.DecideNestApprovalAsync(NestId, Card(), approve: false, default);
            await client.PauseNestAsync(NestId, resume: false, default);
            await client.PauseNestAsync(NestId, resume: true, default);
            await client.RenameNestAsync(NestId, "New name", default);
            await client.DeleteNestFileAsync(NestId, "nfl_0123456789abcdef01234567", default);
            await client.DeleteNestAsync(NestId, "Helper", "Helper", default);
            await client.CreateOrgNestAsync("org_a", "hr", "HR", default);
            var seen = server.Seen.Select(r => $"{r.Method} {r.Path}").ToArray();
            Require(seen.SequenceEqual([
                $"POST /v1/nests/{NestId}/approvals/nap_0123456789abcdef01234567/approve", $"POST /v1/nests/{NestId}/approvals/nap_0123456789abcdef01234567/reject",
                $"POST /v1/nests/{NestId}/pause", $"POST /v1/nests/{NestId}/resume", $"PATCH /v1/nests/{NestId}", $"DELETE /v1/nests/{NestId}/files/nfl_0123456789abcdef01234567",
                $"DELETE /v1/nests/{NestId}", "POST /v1/orgs/org_a/nests"]), string.Join(" | ", seen));
            Require(server.Seen[0].Text == $$"""{"args_hash":"{{Hash}}"}""", server.Seen[0].Text ?? "no body");
            Require(server.Seen[4].Text == """{"label":"New name"}""", "rename body");
            Require(server.Seen[6].Text == """{"confirm":"Helper"}""", "delete body");
            Require(server.Seen[7].Text == """{"handle":"hr","label":"HR"}""", "org body");
        });
        yield return ("Uploads send raw bytes with the encoded name, and deletes need the typed name first", async () =>
        {
            var server = new Postbox();
            using var client = server.Client();
            await client.UploadNestFileAsync(NestId, "a b.txt", [1, 2, 3], default);
            var upload = server.Seen.Single();
            Require(upload is { Method: "POST", Path: $"/v1/nests/{NestId}/files", FileName: "a%20b.txt" } && upload.Bytes!.SequenceEqual(new byte[] { 1, 2, 3 }), "upload");
            await Refused(() => client.UploadNestFileAsync(NestId, "x", new byte[NestRules.MaxUpload + 1], default), "too_large");
            await Refused(() => client.UploadNestFileAsync(NestId, "x", [], default), "too_large");
            await Refused(() => client.DeleteNestAsync(NestId, "Helper", "helper", default), "confirm_required");
            Require(server.Seen.Count == 1, "nothing reached the server without a valid file or the exact name");
            try { await client.NestAsync("nst_../../v1/admin", default); throw new InvalidOperationException("An unsafe id reached a route."); }
            catch (ArgumentException) { }
        });
        yield return ("Refusals arrive with the server's code and exports return the archive", async () =>
        {
            var server = new Postbox();
            using var client = server.Client();
            server.Reply = _ => new HttpResponseMessage(HttpStatusCode.Conflict) { Content = new StringContent("""{"error":"approval_changed"}""", Encoding.UTF8, "application/json") };
            await Refused(() => client.DecideNestApprovalAsync(NestId, Card(), true, default), "approval_changed");
            server.Reply = _ => new HttpResponseMessage(HttpStatusCode.OK) { Content = new ByteArrayContent("PK\u0003\u0004"u8.ToArray()) };
            Require((await client.ExportNestAsync(NestId, default)).Length == 4, "export bytes");
            server.Reply = _ => new HttpResponseMessage(HttpStatusCode.OK) { Content = new StringContent("not json") };
            try { await client.NestSnapshotAsync(default); throw new InvalidOperationException("Accepted a broken answer."); }
            catch (NestWireException) { }
        });
    }

    private static async Task Refused(Func<Task> action, string code)
    {
        try { await action(); }
        catch (PostboxException error) when (error.Code == code) { return; }
        throw new Exception("Nests: expected refusal " + code);
    }

    private static void Require(bool value, string why) { if (!value) throw new Exception("Nests: " + why); }

    private sealed record Request(string Method, string Path, string? Text, byte[]? Bytes, string? FileName);

    private sealed class Postbox : HttpMessageHandler
    {
        public List<Request> Seen { get; } = [];
        public Func<HttpRequestMessage, HttpResponseMessage>? Reply { get; set; }
        public PostboxClient Client() => new(new HttpClient(this), new Tokens(), new Uri("http://127.0.0.1:9"), new HttpClient(this));
        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
        {
            var bytes = request.Content is null ? null : await request.Content.ReadAsByteArrayAsync(cancellationToken);
            var name = request.Headers.TryGetValues("x-file-name", out var values) ? values.Single() : null;
            var json = request.Content?.Headers.ContentType?.MediaType == "application/json" && bytes is not null ? Encoding.UTF8.GetString(bytes) : null;
            Seen.Add(new(request.Method.Method, request.RequestUri!.PathAndQuery, json, bytes, name));
            return Reply?.Invoke(request) ?? new HttpResponseMessage(HttpStatusCode.OK) { Content = new StringContent("{}", Encoding.UTF8, "application/json") };
        }
    }

    private sealed class Tokens : IAccessTokenProvider
    {
        public AuthorizationLease? AcceptedAuthorization { get; } = new();
        public Task<string> GetTokenAsync(CancellationToken ct) => Task.FromResult("fixture-nests");
        public Task<string> RefreshTokenAsync(CancellationToken ct) => throw new Exception("Unexpected renewal");
    }
}
