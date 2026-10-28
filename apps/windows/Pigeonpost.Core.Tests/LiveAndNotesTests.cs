using System.Net;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using Pigeonpost.Core;
using Pigeonpost.Core.Groups;
using Pigeonpost.Core.Meetings;
using Pigeonpost.Core.Preview;

// Native round three: Phase 6 live streams in groups (docs/streaming.md) and Phase 7 meeting notes
// (docs/meeting-summaries.md) on Windows. Synthetic accounts, keys and briefs only.
internal static class LiveAndNotesTests
{
    private static void Require(bool condition, string reason) { if (!condition) throw new Exception(reason); }

    private sealed class Tokens : IAccessTokenProvider
    {
        public AuthorizationLease? AcceptedAuthorization { get; set; } = new();
        public string? RecoveryOwnerKey { get; set; } = Owner("alice");
        public Task<string> GetTokenAsync(CancellationToken token) => Task.FromResult("synthetic-live-notes");
        public Task<string> RefreshTokenAsync(CancellationToken token) => Task.FromResult("synthetic-live-notes");
    }
    private static string Owner(string who) => Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes("synthetic-owner\0" + who))).ToLowerInvariant();

    // Holds matching requests until released, to put a login or selection change in between.
    private sealed class Hold(HttpMessageHandler inner) : DelegatingHandler(inner)
    {
        private Func<HttpRequestMessage, bool>? match;
        private TaskCompletionSource? reached, release;
        public (Task Reached, TaskCompletionSource Release) Pause(Func<HttpRequestMessage, bool> when)
        {
            match = when; reached = new(TaskCreationOptions.RunContinuationsAsynchronously); release = new(TaskCreationOptions.RunContinuationsAsynchronously);
            return (reached.Task, release);
        }
        protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken token)
        {
            if (match is { } m && m(request)) { match = null; reached!.TrySetResult(); await release!.Task; }
            return await base.SendAsync(request, token);
        }
    }

    private sealed class FixedProtector : Pigeonpost.Core.Groups.IStateProtector
    {
        private readonly byte[] key = RandomNumberGenerator.GetBytes(32);
        public Task<byte[]> ProtectAsync(byte[] plain)
        {
            var iv = RandomNumberGenerator.GetBytes(12); var output = new byte[12 + plain.Length + 16];
            using var gcm = new AesGcm(key, 16); gcm.Encrypt(iv, plain, output.AsSpan(12, plain.Length), output.AsSpan(12 + plain.Length));
            iv.CopyTo(output, 0); return Task.FromResult(output);
        }
        public Task<byte[]> UnprotectAsync(byte[] sealedBytes)
        {
            var plain = new byte[sealedBytes.Length - 28];
            using var gcm = new AesGcm(key, 16); gcm.Decrypt(sealedBytes.AsSpan(0, 12), sealedBytes.AsSpan(12, plain.Length), sealedBytes.AsSpan(12 + plain.Length), plain);
            return Task.FromResult(plain);
        }
    }

    private static byte[] SampleBrief(string meetingId, string title) => JsonSerializer.SerializeToUtf8Bytes(new
    {
        v = 1, meeting_id = meetingId, title, started_at = "2026-10-27T09:00:00Z", duration_s = 600, participants = new[] { "Alice", "Bob" },
        summary = "Synthetic summary.", decisions = new[] { "Ship it" }, action_items = new[] { new { owner = "Bob", task = "Write notes", due = (string?)null } },
        open_questions = Array.Empty<string>(), draft = false, transcript = new[] { new { at = "00:03", speaker = "Alice", text = "Hello" } },
        generated_by = new { stt = "whisper.cpp", llm = "local" },
    });

    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        // ---- streams --------------------------------------------------------------------------

        yield return ("Live stream: LIVE banner, permission-aware Go live, stage invitation, who-can-go-live and the stream notice", async () =>
        {
            var server = new PreviewPostbox();
            server.Features.Add(ServerFeatures.Streaming);
            var alice = server.AddMailbox(PreviewPostbox.Seed(81), "/alice/main");
            var bob = server.AddMailbox(PreviewPostbox.Seed(82), "/bob/main");
            using var client = new PostboxClient(new HttpClient(server), new Tokens(), new Uri("http://127.0.0.1:9"));
            var features = await client.GetCapabilitiesAsync(default);
            using var a = new GroupsModel(client, _ => new MemoryGroupStateStore());
            using var b = new GroupsModel(client, _ => new MemoryGroupStateStore());
            await a.ActivateAsync(new Mailbox(alice, "/alice/main"), features, [new Mailbox(alice, "/alice/main")], []);
            await b.ActivateAsync(new Mailbox(bob, "/bob/main"), features, [new Mailbox(bob, "/bob/main")], [new Contact("/alice/main", "Alice A.", "allow", "review")]);
            var created = await a.CreateAsync(new GroupSettings("Crew"), ["/bob/main"]);
            Require(created is not null && GroupStreams.ValidGroupId(created.Id), "Group ids have the postbox's form: " + created?.Id);
            await b.RefreshAsync(); await b.SelectAsync(b.Groups.Single()); await b.AnswerInvitationAsync(true);
            await a.RefreshAsync(); await b.RefreshAsync(); await b.SelectAsync(b.Groups.Single());
            await a.RefreshStreamAsync(); await b.RefreshStreamAsync();
            Require(a.Streaming && a.ShowsGoLive && a.CanChooseWhoGoesLive && !a.ShowsLiveBanner, "The owner may go live under the default admins policy.");
            Require(b.Streaming && !b.ShowsGoLive && !b.CanChooseWhoGoesLive, "A member may not go live while only admins can.");

            await a.SetStreamPolicyAsync(everyone: true);
            await b.RefreshAsync(); await b.SelectAsync(b.Groups.Single());
            Require(b.Info?.StreamPolicy == "everyone" && b.ShowsGoLive, "Everyone may go live after the admin's change.");

            var stream = server.StartStream(created!.Id, bob, "Sprint\u0007 demo");
            await a.RefreshStreamAsync(); await b.RefreshStreamAsync();
            Require(a.ShowsLiveBanner && a.LiveBannerText == "Bob is live: Sprint  demo" && !a.ShowsGoLive, "Others see who is live: " + a.LiveBannerText);
            Require(b.LiveIsMine && b.LiveBannerText == "You are live: Sprint  demo" && !b.ShowsGoLive, "The streamer sees their own stream: " + b.LiveBannerText);
            Require(a.StageInviteText is null, "No stage invitation yet.");

            server.SetStage(stream, alice, "invited", bob);
            await a.RefreshStreamAsync();
            Require(a.StageInviteText == "Bob invited you onto the stage. Your microphone and camera turn on only if you join.", "Stage invitation: " + a.StageInviteText);
            var status = await a.StreamStatusAsync(stream);
            Require(status is { IsLive: true } && GroupStreams.StageOf(status, alice) == "invited" && GroupStreams.StageOf(status, bob) is null, "The watched stream's record carries the stage.");

            server.EndStream(stream, 2040, 18);
            await a.RefreshAsync(); await a.RefreshStreamAsync();
            Require(!a.ShowsLiveBanner && a.ShowsGoLive, "The banner leaves with the stream.");
            Require(a.Lines.Any(l => l.Text == "Bob streamed for 34 min · 18 viewers"), "Stream notice: " + string.Join(" | ", a.Lines.Select(l => l.Text)));
            Require(await a.StreamStatusAsync(stream) is null, "An ended stream has no live status.");
        });

        yield return ("Live streams stay hidden without the flag, and a stream answer for another group, inbox or login is dropped", async () =>
        {
            var server = new PreviewPostbox();
            var alice = server.AddMailbox(PreviewPostbox.Seed(83), "/alice/main");
            var hold = new Hold(server);
            var tokens = new Tokens();
            using var client = new PostboxClient(new HttpClient(hold), tokens, new Uri("http://127.0.0.1:9"));
            using var a = new GroupsModel(client, _ => new MemoryGroupStateStore());
            var me = new Mailbox(alice, "/alice/main");
            await a.ActivateAsync(me, await client.GetCapabilitiesAsync(default), [me], []);
            var first = await a.CreateAsync(new GroupSettings("One"), []);
            await a.RefreshStreamAsync();
            Require(!a.Streaming && !a.ShowsGoLive && !server.Requests.Any(r => r.EndsWith("/streams", StringComparison.Ordinal)), "Without `streaming` nothing is asked or shown.");

            server.Features.Add(ServerFeatures.Streaming);
            await a.ActivateAsync(me, await client.GetCapabilitiesAsync(default), [me], []);
            await a.RefreshAsync();
            var second = await a.CreateAsync(new GroupSettings("Two"), []);
            server.StartStream(first!.Id, alice, "In one");
            await a.SelectAsync(a.Groups.Single(g => g.Id == first.Id));
            var (reached, release) = hold.Pause(r => r.RequestUri!.AbsolutePath.EndsWith("/streams", StringComparison.Ordinal));
            var pending = a.RefreshStreamAsync(); await reached;
            await a.SelectAsync(a.Groups.Single(g => g.Id == second!.Id));
            release.TrySetResult(); await pending;
            Require(!a.ShowsLiveBanner && a.Selected?.Id == second!.Id, "A stream read for the previous group does not appear on the next one.");

            Require(GroupStreams.LiveOf(second!.Id, [new GroupStream { Id = "str_" + new string('a', 26), GroupId = first.Id, State = "live" }]) is null, "A stream naming another group is ignored.");
            Require(GroupStreams.LiveOf(second.Id, [new GroupStream { Id = "str_bad", GroupId = second.Id, State = "live" }]) is null, "A malformed stream id is ignored.");

            await a.SelectAsync(a.Groups.Single(g => g.Id == first.Id));
            (reached, release) = hold.Pause(r => r.RequestUri!.AbsolutePath.EndsWith("/streams", StringComparison.Ordinal));
            pending = a.RefreshStreamAsync(); await reached;
            tokens.AcceptedAuthorization!.Retire(); tokens.AcceptedAuthorization = new AuthorizationLease();
            await a.ActivateAsync(me, await client.GetCapabilitiesAsync(default), [me], []);
            release.TrySetResult(); await pending;
            Require(!a.ShowsLiveBanner && a.LiveStream is null, "A stream read under a replaced login is not shown.");
        });

        yield return ("Stream bridge: requests carry only validated JSON and answers are typed and bounded", () =>
        {
            var group = "grp_" + new string('b', 26); var stream = "str_" + new string('c', 26); var op = Guid.NewGuid();
            var watch = CallBridge.WatchStreamScript(group, "Crew \"</script>", stream, op);
            Require(watch.Contains("native.watchStream({\"id\":\"" + group + "\",\"name\":\"Crew \\u0022\\u003C/script\\u003E\"}, \"" + stream + "\")", StringComparison.Ordinal)
                && watch.Contains(op.ToString("N"), StringComparison.Ordinal), "Watch request: " + watch);
            var live = CallBridge.GoLiveScript(group, "Crew", "Demo'); alert(1); ('", camera: true, screen: false, op);
            Require(live.Contains("\"title\":\"Demo\\u0027); alert(1); (\\u0027\"", StringComparison.Ordinal) && live.Contains("\"camera\":true,\"screen\":false", StringComparison.Ordinal), "Go live request: " + live);
            foreach (var bad in new Action[]
            {
                () => CallBridge.WatchStreamScript("grp_1", "Crew", stream, op), () => CallBridge.WatchStreamScript(group, "Crew", "str_x", op),
                () => CallBridge.WatchStreamScript(group, "Crew", stream, Guid.Empty), () => CallBridge.GoLiveScript(group, "Crew", "  ", true, false, op),
                () => CallBridge.GoLiveScript(group, "Crew", "Demo", false, false, op), () => CallBridge.StageScript(stream, "/k/me", "speaker", null),
            })
            {
                try { bad(); throw new Exception("An invalid stream request was built."); } catch (ArgumentException) { }
            }
            Require(CallBridge.StageScript(stream, "/k/me", "invited", "/k/host").Contains("\"stage\":{\"address\":\"/k/me\",\"state\":\"invited\",\"by\":\"/k/host\"}", StringComparison.Ordinal), "Stage forwarding.");
            var ok = CallBridge.Parse("{\"type\":\"stream_result\",\"operation\":\"" + op.ToString("N") + "\",\"result\":{\"ok\":true,\"stream\":\"" + stream + "\"}}");
            Require(ok is { Type: "stream_result", State: "ok", ErrorCode: null } && ok.Operation == op && ok.Stream == stream, "A successful answer.");
            var refused = CallBridge.Parse("{\"type\":\"stream_result\",\"operation\":\"" + op.ToString("N") + "\",\"result\":{\"ok\":false,\"code\":\"stream restricted<b>\",\"stream\":\"str_nope\"}}");
            Require(refused is { State: "error", ErrorCode: "unavailable", Stream: null }, "A malformed code or stream id never reaches the window.");
            Require(CallBridge.Parse("""{"type":"stream_result","result":{"ok":true}}""") is null, "An answer without its operation is ignored.");
            Require(GroupStreams.Explain("stream_capacity").StartsWith("Live streams are very busy", StringComparison.Ordinal), "Capacity is explained.");
            Require(GroupStreams.NoticeText("Jane", new Dictionary<string, string> { ["duration"] = "29", ["viewers"] = "1" }) == "Jane streamed for 1 min · 1 viewer"
                && GroupStreams.NoticeText("Jane", new Dictionary<string, string> { ["duration"] = "90", ["viewers"] = "x" }) == "Jane streamed for 2 min · 0 viewers", "Notice wording.");
            return Task.CompletedTask;
        });

        // ---- meeting notes --------------------------------------------------------------------

        yield return ("Meeting notes: a brief sealed by meeting-intel's own code opens on Windows, and nothing else does", () =>
        {
            using var fixture = JsonDocument.Parse(File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "Fixtures", "meeting-brief.json")));
            var k = fixture.RootElement.GetProperty("key");
            using var key = NotesDeviceKey.Restore(k.GetProperty("key_id").GetString()!, k.GetProperty("public_key").GetString()!, Convert.FromBase64String(k.GetProperty("pkcs8").GetString()!));
            var record = fixture.RootElement.GetProperty("record").Deserialize<MeetingRecord>(PostboxJson.Options)!;
            var brief = MeetingNotesCrypto.Open(record, key);
            Require(brief.Heading == "Synthetic sprint review" && brief.Summary!.StartsWith("The team reviewed", StringComparison.Ordinal), "Title and summary.");
            Require(brief.Decisions.Single() == "Ship 1.0.11 with the batched release" && brief.OpenQuestions.Single() == "Which day does the batch go out?", "Decisions and questions.");
            Require(brief.ActionItems.Select(a => a.Text).SequenceEqual(["Alice: Prepare the store notes (due Friday)", "Check the screenshots"]), "Action items.");
            Require(brief.Transcript.Count == 2 && brief.Transcript[1].Text == "Ekran görüntüleri hazır." && brief.DurationSeconds == 754 && brief.Participants.Count == 2, "Transcript.");
            var text = brief.AsText();
            Require(text.StartsWith("Synthetic sprint review\n\nThe team reviewed", StringComparison.Ordinal) && text.Contains("- [00:12] Bob: Ekran görüntüleri hazır.", StringComparison.Ordinal)
                && text.Contains("Action items:\n- Alice: Prepare the store notes (due Friday)", StringComparison.Ordinal), "Copy as text:\n" + text);
            Require(brief.GeneratedLine.Contains("(whisper.cpp ggml-small, qwen-latest-flash-next)", StringComparison.Ordinal), "Generated line.");

            string Code(Func<MeetingBrief> open) { try { open(); return "opened"; } catch (MeetingNotesException ex) { return ex.Code; } }
            var bytes = Convert.FromBase64String(record.Ciphertext!); bytes[5] ^= 1;
            Require(Code(() => MeetingNotesCrypto.Open(record with { Ciphertext = Convert.ToBase64String(bytes) }, key)) == "bad_brief", "A changed ciphertext is refused.");
            Require(Code(() => MeetingNotesCrypto.Open(record with { Id = "mtg_" + new string('z', 26) }, key)) == "bad_brief", "A brief moved to another meeting is refused.");
            using var other = NotesDeviceKey.Create();
            Require(Code(() => MeetingNotesCrypto.Open(record, other)) == "no_key_for_device", "Another device's key has no wrap.");
            var swapped = record with { Keys = record.Keys!.Select(w => w with { KeyId = w.KeyId == key.KeyId ? "nkSyntheticOther1" : key.KeyId }).ToArray() };
            Require(Code(() => MeetingNotesCrypto.Open(swapped, key)) == "bad_brief", "A wrap made for another key id does not open under this one.");
            Require(Code(() => MeetingNotesCrypto.Open(record with { State = "processing" }, key)) == "not_ready", "Only ready notes open.");
            var tampered = MeetingNotesCrypto.Seal(record.Id, Encoding.UTF8.GetBytes("{\"v\":1,\"meeting_id\":\"mtg_" + new string('y', 26) + "\"}"), [(key.KeyId, key.PublicKey)]);
            Require(Code(() => MeetingNotesCrypto.Open(record with { Iv = tampered.Iv, Ciphertext = tampered.Ciphertext, Keys = tampered.Wraps }, key)) == "bad_brief", "A brief naming another meeting is refused.");
            var hostile = MeetingNotesCrypto.Seal(record.Id, Encoding.UTF8.GetBytes("{\"title\":\"A\\u202Eevil\\u0000\",\"summary\":\"line one\\nline two\"}"), [(key.KeyId, key.PublicKey)]);
            var cleaned = MeetingNotesCrypto.Open(record with { Iv = hostile.Iv, Ciphertext = hostile.Ciphertext, Keys = hostile.Wraps }, key);
            Require(cleaned.Title == "A evil" && cleaned.Summary == "line one\nline two" && cleaned.Decisions.Count == 0, "Controls and direction overrides are removed: " + cleaned.Title);
            return Task.CompletedTask;
        });

        yield return ("Meeting notes keys: sealed at rest, bound to their person, restored only as a matching pair", async () =>
        {
            var folder = Directory.CreateTempSubdirectory("pp-notes-keys-").FullName;
            try
            {
                var protector = new FixedProtector();
                var store = new FileNotesKeyStore(folder, protector);
                using var key = NotesDeviceKey.Create();
                Require(NotesDeviceKey.ValidKeyId(key.KeyId) && Convert.FromBase64String(key.PublicKey).Length == 65, "A notes key looks like the browser's: " + key.KeyId);
                await store.SaveAsync(Owner("alice"), key);
                var file = Directory.GetFiles(folder).Single();
                var saved = await File.ReadAllBytesAsync(file);
                Require(!Encoding.UTF8.GetString(saved).Contains(key.KeyId, StringComparison.Ordinal) && Path.GetFileName(file) == Owner("alice") + ".key", "The key file is sealed and named by the person's hash only.");
                using var loaded = await store.LoadAsync(Owner("alice"));
                Require(loaded is { } l && l.KeyId == key.KeyId && l.PublicKey == key.PublicKey, "The same person gets their key back.");
                Require(await store.LoadAsync(Owner("bob")) is null, "Another person has no key here.");
                File.Copy(file, Path.Combine(folder, Owner("bob") + ".key"));
                try { await store.LoadAsync(Owner("bob")); throw new Exception("A key saved for another person was accepted."); }
                catch (MeetingNotesException ex) when (ex.Code == "bad_key") { }
                try { await store.SaveAsync("../escape", key); throw new Exception("A path-like owner was accepted."); } catch (ArgumentException) { }
                using var stranger = NotesDeviceKey.Create();
                using var unrelated = ECDiffieHellman.Create(ECCurve.NamedCurves.nistP256);
                try { NotesDeviceKey.Restore(key.KeyId, stranger.PublicKey, unrelated.ExportPkcs8PrivateKey()); throw new Exception("A mismatched key pair was restored."); }
                catch (MeetingNotesException ex) when (ex.Code == "bad_key") { }
                await store.RemoveAsync(Owner("alice"));
                Require(await store.LoadAsync(Owner("alice")) is null, "Removing forgets the key.");
            }
            finally { Directory.Delete(folder, true); }
        });

        yield return ("Meeting notes screen: registers this device's key, lists, opens, explains and deletes, for one login only", async () =>
        {
            var server = new PreviewPostbox();
            server.Features.Add(ServerFeatures.MeetingSummaries);
            var hold = new Hold(server);
            var tokens = new Tokens();
            using var client = new PostboxClient(new HttpClient(hold), tokens, new Uri("http://127.0.0.1:9"));
            var store = new MemoryNotesKeyStore();
            using var model = new MeetingNotesModel(client, store);
            var features = await client.GetCapabilitiesAsync(default);
            model.Activate(ServerFeatures.None);
            Require(!model.Enabled, "Off without the flag.");
            model.Activate(features);
            var ready = server.AddMeeting("ready", "/k/synthetichost", null, ago: 100);
            server.AddMeeting("processing", "/k/synthetichost", ago: 50);
            var other = server.AddMeeting("ready", "/k/synthetichost", id => SampleBrief(id, "Elsewhere"), otherDevice: true, ago: 200);
            var failed = server.AddMeeting("failed", "/k/synthetichost", failure: "no_speech", ago: 300);
            await model.LoadAsync();
            Require(model.Enabled && model.Meetings.Count == 4 && server.NotesKeys.Single().Value.Label == "Windows app" && store.Owners.Single() == Owner("alice"),
                "Loading registers one key for this person and lists every meeting.");
            Require(model.Meetings.First(m => m.Id == ready).Detail.StartsWith("Ready · host", StringComparison.Ordinal), "Row wording: " + model.Meetings.First().Detail);

            Require((await model.OpenAsync(ready))?.Message == "This device couldn't open these notes.", "A ready record without content cannot be opened.");
            Require((await model.OpenAsync(model.Meetings.Single(m => m.Meeting.State == "processing").Id))?.Message == "The notes are still being prepared. Check back in a few minutes.", "Processing explains itself.");
            Require((await model.OpenAsync(failed))?.Message == "No speech was captured.", "A failure is explained.");
            Require((await model.OpenAsync(other))?.Message?.StartsWith("These notes were encrypted for your other devices.", StringComparison.Ordinal) == true, "Another device's notes say so.");
            var misfiled = server.AddMeeting("ready", "/k/synthetichost", _ => SampleBrief("mtg_" + new string('9', 26), "Elsewhere"), ago: 2);
            Require((await model.OpenAsync(misfiled))?.Message == "This device couldn't open these notes.", "A brief naming another meeting is refused even when it decrypts.");
            var good = server.AddMeeting("ready", "/k/synthetichost", id => SampleBrief(id, "Design review"), ago: 0);
            var result = await model.OpenAsync(good);
            Require(result?.Brief is { Heading: "Design review" } brief && brief.ActionItems.Single().Text == "Bob: Write notes", "The brief opens: " + result?.Message);

            Require(await model.DeleteAsync(failed) && model.Meetings.All(m => m.Id != failed), "Delete my copy removes the row.");

            // Another login: nothing of the first carries over, and the first person's key is not used.
            var (reached, release) = hold.Pause(r => r.Method == HttpMethod.Get && r.RequestUri!.AbsolutePath == "/v1/meetings/" + good);
            var late = model.OpenAsync(good); await reached;
            tokens.AcceptedAuthorization!.Retire(); tokens.AcceptedAuthorization = new AuthorizationLease(); tokens.RecoveryOwnerKey = Owner("bob");
            model.Activate(features);
            release.TrySetResult();
            Require(await late is null && model.Meetings.Count == 0, "An answer for the previous login is dropped and its list is gone.");
            await model.LoadAsync();
            Require(store.Owners.Count == 2 && server.NotesKeys.Count == 2, "The new person gets their own key: " + string.Join(",", server.NotesKeys.Keys));
            Require((await model.OpenAsync(good))?.Message?.StartsWith("These notes were encrypted for your other devices.", StringComparison.Ordinal) == true,
                "The second person's key cannot open notes wrapped for the first.");

            model.Reset();
            Require(!model.Enabled && model.Meetings.Count == 0 && await model.OpenAsync(good) is null, "Sign-out leaves nothing to show.");
        });

        yield return ("Meeting notes: a key id the postbox holds for a different key is replaced by a new device key", async () =>
        {
            var server = new PreviewPostbox();
            server.Features.Add(ServerFeatures.MeetingSummaries);
            var tokens = new Tokens();
            using var client = new PostboxClient(new HttpClient(server), tokens, new Uri("http://127.0.0.1:9"));
            var store = new MemoryNotesKeyStore();
            using var saved = NotesDeviceKey.Create();
            await store.SaveAsync(Owner("alice"), saved);
            using var squatter = NotesDeviceKey.Create();
            using (var put = new HttpClient(server))
            {
                var response = await put.PutAsync("http://127.0.0.1:9/v1/meetings/keys/" + saved.KeyId,
                    new StringContent(JsonSerializer.Serialize(new { public_key = squatter.PublicKey }), Encoding.UTF8, "application/json"));
                Require(response.StatusCode == HttpStatusCode.OK, "Setup.");
            }
            using var model = new MeetingNotesModel(client, store);
            model.Activate(await client.GetCapabilitiesAsync(default));
            await model.RegisterAsync();
            using var now = await store.LoadAsync(Owner("alice"));
            Require(now!.KeyId != saved.KeyId && server.NotesKeys.ContainsKey(now.KeyId) && server.NotesKeys[saved.KeyId].PublicKey == squatter.PublicKey && model.DeviceKeyId == now.KeyId,
                "A new key is made, saved and registered; the conflicting id is left alone.");
        });
    }

}
