using Pigeonpost.Core;
using Pigeonpost.Core.Preview;

// Phase 1b on Windows: filtering, blocking, call history, missed-call lines, profile pictures and
// direct-thread timers, against the stand-in postbox (same rules as the web reference).
internal static class PrivacyTests
{
    private sealed class Tokens : IAccessTokenProvider
    {
        public Task<string> GetTokenAsync(CancellationToken token) => Task.FromResult("test");
        public Task<string> RefreshTokenAsync(CancellationToken token) => Task.FromResult("test");
    }

    private static void Require(bool condition, string reason) { if (!condition) throw new Exception(reason); }
    private static void Equal<T>(T actual, T expected) => Require(EqualityComparer<T>.Default.Equals(actual, expected), $"Expected {expected}, got {actual}.");

    private static (PreviewPostbox Server, PostboxClient Client, string Me) Setup()
    {
        var server = new PreviewPostbox();
        var me = server.AddMailbox(PreviewPostbox.Seed(90), "/me/main");
        return (server, new PostboxClient(new HttpClient(server), new Tokens(), new Uri("http://127.0.0.1:9")), me);
    }

    private static CallLogEntry Call(string id, string direction, string outcome, long at, string? handle = "/alp/main", string kind = "audio", string? group = null) => new()
    {
        Id = id, Direction = direction, Outcome = outcome, Kind = kind, Peer = handle is null ? null : "/k/peer" + id, PeerHandle = handle, StartedAt = at, GroupId = group,
        AnsweredAt = outcome == "answered" ? at + 2 : null, EndedAt = outcome == "answered" ? at + 125 : null,
        Participants = group is null ? ["/k/a", "/k/b"] : ["/k/a", "/k/b", "/k/c"],
    };

    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Capabilities gate every Phase 1b feature and fail closed", async () =>
        {
            var (server, client, _) = Setup();
            var features = await client.GetCapabilitiesAsync(default);
            Require(features.Has(ServerFeatures.CallHistory) && features.Has(ServerFeatures.Avatars), "Enabled features are listed.");
            server.Features.Clear();
            server.Features.Add(ServerFeatures.CallHistory);
            Equal(PrivacySettings.For(await client.GetCapabilitiesAsync(default)).Single().Field, "call_history_days");
            using var broken = new PostboxClient(new HttpClient(new Fixed(System.Net.HttpStatusCode.InternalServerError)), new Tokens(), new Uri("http://127.0.0.1:9"));
            Require((await broken.GetCapabilitiesAsync(default)).Names.Count == 0, "A failing server enables nothing.");
        });

        yield return ("Privacy and filtering settings load, save one field at a time and refuse bad values", async () =>
        {
            var (server, client, me) = Setup();
            var policy = await client.GetPolicyAsync(me, default);
            Equal(policy.UnnamedCalls, "silence"); Equal(policy.CallHistoryDays, 180); Equal(policy.Value("call_history_days"), "180");
            policy = await client.SetPolicyAsync(me, "unknown_messages", PrivacySettings.Encode("unknown_messages", "filter"), default);
            Equal(policy.UnknownMessages, "filter");
            policy = await client.SetPolicyAsync(me, "call_history_days", PrivacySettings.Encode("call_history_days", "30"), default);
            Equal(policy.CallHistoryDays, 30);
            Equal((string?)server.PolicyOf(me)["unknown_messages"], "filter");
            try { await client.SetPolicyAsync(me, "call_history_days", 7, default); throw new Exception("accepted"); }
            catch (PostboxException ex) { Equal(ex.Code, "invalid_policy"); }
            Equal(PrivacySettings.All.Count, 6);
            Require(PrivacySettings.All.All(s => s.Options.Count >= 2), "Every setting offers choices.");
        });

        yield return ("Block rules: confirmation wording, wildcards only for owned namespaces, blocked list and unblock", async () =>
        {
            Equal(BlockRules.Confirmation, "They won't be able to message or call you, and won't be told.");
            Equal(BlockRules.WildcardFor("/spamco/agent"), "/spamco/*");
            Equal(BlockRules.WildcardFor("/spamco"), "/spamco/*");
            Equal(BlockRules.WildcardFor("/github/alex/main"), "/github/alex/*");
            Equal(BlockRules.WildcardFor("/github/alex"), null);
            Equal(BlockRules.WildcardFor("/k/abc"), null);
            Equal(BlockRules.WildcardFor("/pp/x"), null);
            Require(BlockRules.IsBlockable("/spamco/*") && BlockRules.IsBlockable("/k/abc") && !BlockRules.IsBlockable("/*") && !BlockRules.IsBlockable("spamco"), "Blockable addresses.");
            Equal(BlockRules.Describe(new Contact("/spamco/*", null, "block", "review")), "Everyone at /spamco");
            Require(BlockRules.IsBlocked("/spamco/agent", [new Contact("/spamco/*", null, "block", "review")]), "A wildcard blocks the namespace.");

            var (server, client, me) = Setup();
            await client.BlockAsync(me, "/spamco/*", null, default);
            await client.BlockAsync(me, "/k/stranger", "Stranger", default);
            var blocked = await client.GetBlockedAsync(me, default);
            Equal(blocked.Count, 2);
            await client.UnblockAsync(me, "/spamco/*", default);
            Equal(server.BlockedOf(me).Single().Peer, "/k/stranger");
        });

        yield return ("Filtered senders group by sender, leave out blocked ones, and Accept moves them out", async () =>
        {
            var (server, client, me) = Setup();
            server.AddFiltered(me, new InboxMessage { MessageId = "f1", Body = "Buy now", From = "/k/spam1", ReceivedAt = 10 });
            server.AddFiltered(me, new InboxMessage { MessageId = "f2", Body = "Hello?", From = "/k/new", SenderHandle = "/new/main", ReceivedAt = 20 });
            server.AddFiltered(me, new InboxMessage { MessageId = "f3", Body = "Again", From = "/k/new", SenderHandle = "/new/main", ReceivedAt = 30 });
            server.AddFiltered(me, new InboxMessage { MessageId = "f4", Body = "Blocked", From = "/k/gone", ReceivedAt = 40 });
            var filtered = await client.GetFilteredAsync(me, default);
            var senders = FilteredSender.Group(filtered, [new Contact("/k/gone", null, "block", "review")]);
            Equal(string.Join(",", senders.Select(s => s.Peer)), "/new/main,/k/spam1");
            Equal(senders[0].CountLabel, "2 messages"); Equal(senders[0].Preview, "Again"); Equal(senders[0].Name, "/new");
            await client.AcceptFilteredAsync(me, "/new/main", default);
            Equal(FilteredSender.Group(await client.GetFilteredAsync(me, default), []).Count, 2);
            await client.ReportMessageAsync(me, "f1", default);
            Require(server.Reports.Contains((me, "message", "f1")), "Spam reports reach the postbox.");
        });

        yield return ("Calls page: newest first, Missed filter, Filtered section, badge cleared on view, delete, report, clear", async () =>
        {
            var (server, client, me) = Setup();
            server.AddCall(me, Call("c1", "incoming", "missed", 100, kind: "video"));
            server.AddCall(me, Call("c2", "outgoing", "answered", 200));
            server.AddCall(me, Call("c3", "incoming", "filtered", 300, "/k/unknown"));
            server.AddCall(me, Call("c4", "incoming", "missed", 400, handle: null, group: "grp_x"));
            var model = new CallHistoryModel(client);
            model.Reset(me);
            await model.LoadAsync();
            Equal(string.Join(",", model.Entries.Select(e => e.Id)), "c4,c2,c1");
            Equal(model.FilteredEntries.Single().Id, "c3");
            Equal(model.FilteredTitle, "Filtered calls (1)");
            Equal(model.UnseenMissed, 2); Equal(model.AccessibleName, "Calls, 2 missed"); Equal(model.BadgeLabel, "2");
            await model.MarkSeenAsync();
            Equal(model.UnseenMissed, 0);
            await model.LoadAsync();
            Equal(model.UnseenMissed, 0);
            model.Filter = "missed";
            await model.LoadAsync();
            Equal(string.Join(",", model.Entries.Select(e => e.Id)), "c4,c1");

            var missed = CallRow.For(model.Entries.Single(e => e.Id == "c1"), [new Contact("/alp/main", "Alp", "allow", "review")]);
            Equal(missed.Name, "Alp"); Require(missed.IsMissed && missed.CanCallBack && missed.CanReport, "Missed incoming row actions.");
            Require(missed.Detail.StartsWith("↙ Missed · Video", StringComparison.Ordinal), "Row detail: " + missed.Detail);
            var group = CallRow.For(model.Entries.Single(e => e.Id == "c4"), []);
            Equal(group.Name, "3 people"); Require(!group.CanCallBack && group.Detail.EndsWith("3 people", StringComparison.Ordinal), "Group call row.");
            var answered = CallRow.For(Call("x", "outgoing", "answered", 1), []);
            Require(answered.Detail == "↗ Outgoing · Voice · 2:03", "Answered detail: " + answered.Detail);
            Equal(CallPresentation.Duration(3725), "1:02:05");

            Equal(await model.ReportAsync(model.Entries[0]), "Reported as spam.");
            Require(server.Reports.Any(r => r.Kind == "call"), "Call report sent with the call id.");
            Equal(await model.DeleteAsync(model.Entries[0]), "Removed from call history.");
            Equal(model.Entries.Count, 1);
            Equal(await model.ClearAsync(), "Call history cleared.");
            Require(model.Entries.Count == 0 && model.FilteredEntries.Count == 0, "Clear all empties both lists.");
        });

        yield return ("A missed 1:1 call adds a system line to that conversation only", () =>
        {
            var acting = new Mailbox("/k/me", "/me/main");
            var snapshot = new InboxSnapshot(
                [new InboxMessage { MessageId = "m1", Body = "Hi", From = "/k/alp", SenderHandle = "/alp/main", ThreadId = "t1", ReceivedAt = 50, Read = true }],
                [new ServerThread("t1", "/alp/main", IsDefault: true)], [], new HashSet<string>());
            var calls = new[] { Call("c1", "incoming", "missed", 100, kind: "video"), Call("c2", "outgoing", "missed", 110), Call("c3", "incoming", "missed", 120, group: "grp_x") };
            var row = ConversationBuilder.Build(snapshot, [], [acting], acting, calls: calls).Single();
            Equal(row.Messages.Count, 2);
            var line = row.Messages.Last();
            Require(line.IsSystem && line.Body.StartsWith("Missed video call · ", StringComparison.Ordinal) && line.ThreadId == "t1", "Missed-call line: " + line.Body);
            Equal(row.Preview, "Hi");
            Equal(row.Unread, 0);
            Equal(ConversationBuilder.Subjects(row, snapshot).Single().Messages.Count, 2);
            return Task.CompletedTask;
        });

        yield return ("Disappearing direct messages leave by the local clock and the timer is set per thread", async () =>
        {
            var acting = new Mailbox("/k/me", "/me/main");
            var snapshot = new InboxSnapshot(
            [
                new InboxMessage { MessageId = "old", Body = "gone", From = "/k/alp", SenderHandle = "/alp/main", ReceivedAt = 10, ExpiresAt = 99 },
                new InboxMessage { MessageId = "new", Body = "stays", From = "/k/alp", SenderHandle = "/alp/main", ReceivedAt = 20, ExpiresAt = 1000 },
                new InboxMessage { MessageId = "filtered", Body = "hidden", From = "/k/alp", SenderHandle = "/alp/main", ReceivedAt = 30, Filtered = true },
            ], [new ServerThread("t1", "/alp/main", IsDefault: true, EphemeralSecs: 3600)], [], new HashSet<string>());
            var row = ConversationBuilder.Build(snapshot, [], [acting], acting, now: 100).Single();
            Equal(string.Join(",", row.Messages.Select(m => m.Id)), "new");
            Require(row.Messages.Single().Disappears, "Messages with a timer are marked.");
            var subject = ConversationBuilder.Subjects(row, snapshot).Single();
            Equal(subject.EphemeralSecs, 3600); Equal(subject.TimerLabel, "1 hour");
            Equal(Timers.Banner(3600), "Messages disappear after 1 hour");

            var (server, client, me) = Setup();
            await client.SetThreadTimerAsync(me, "t1", 86400, default);
            Equal(server.TimerOf(me, "t1"), 86400);
            await client.SetThreadTimerAsync(me, "t1", null, default);
            Equal(server.TimerOf(me, "t1"), null);
        });

        yield return ("Profile pictures: upload with crop and visibility, lookups by address, cached images, visibility and removal", async () =>
        {
            var (server, client, me) = Setup();
            Equal(AvatarCrop.Square(400, 300), (50, 0, 300));
            Equal(AvatarCrop.Square(400, 300, zoom: 2, panX: 0, panY: 1), (0, 150, 150));
            Equal(AvatarCrop.Query(50, 0, 300), "50,0,300,300");
            var jpeg = new byte[] { 0xFF, 0xD8, 0xFF, 0xE0, 1, 2, 3, 0xFF, 0xD9 };
            var own = await client.SetAvatarAsync(me, jpeg, "image/jpeg", (0, 0, 10), "everyone", all: false, default);
            Require(own is { Visibility: "everyone", DisplayUrl: { } url } && url.EndsWith("/256.jpg", StringComparison.Ordinal), "Own picture returned with its URLs.");
            var directory = new AvatarDirectory(client) { Enabled = true };
            var changed = new List<string>();
            directory.PictureChanged += (_, key) => changed.Add(key);
            await directory.RefreshAsync(["/me/main", "/nobody/main"], default);
            Require(directory.Known("/me") is not null && directory.Known("/nobody/main") is null, "Lookups by handle, unknown addresses stay initials.");
            Equal(changed.Single(), "/me/main");
            Require((await directory.ImageAsync("/me/main", default))!.SequenceEqual(jpeg), "Image bytes cached by hash.");
            var visibility = await client.SetAvatarVisibilityAsync(me, "nobody", default);
            Require(visibility!.Version == own!.Version + 1, "A visibility change bumps the version.");
            await directory.RefreshAsync(["/me/main"], default, force: true);
            Require(directory.Known("/me/main") is null, "A hidden picture looks like no picture.");
            await client.RemoveAvatarAsync(me, default);
            Require(!server.HasAvatar(me) && await client.GetOwnAvatarAsync(me, default) is null, "Removal goes back to initials.");
            await client.ReportAvatarAsync(me, "/k/rude", default);
            Require(server.Reports.Contains((me, "avatar", "/k/rude")), "Picture reports reach the operator queue.");
            try { await client.SetAvatarAsync(me, new byte[11 * 1024 * 1024], "image/jpeg", null, "known", false, default); throw new Exception("accepted"); }
            catch (PostboxException ex) { Equal(ex.Code, "too_large"); }
        });

        yield return ("Blocking from a conversation disables the composer, shows the banner and can report", async () =>
        {
            var service = new PreviewInboxService();
            using var vm = new InboxViewModel(service);
            await vm.InitializeAsync();
            vm.SelectConversation(vm.Conversations.Single(c => c.Peer == "/preview/design"));
            Require(vm.CanCompose && !vm.IsSelectedBlocked, "Starts unblocked.");
            Require(await vm.BlockAsync("/preview/design", "Design", report: true), "Block succeeded.");
            vm.SelectConversation(vm.AllConversations.Single(c => c.Peer == "/preview/design"));
            Require(vm.IsSelectedBlocked && !vm.CanCompose, "Blocked conversation keeps its history but not its composer.");
            Equal(vm.BlockedBanner, "You blocked this person. They can't message or call you.");
            Require(service.Reported.Contains("design-3"), "Also report sends the newest received message.");
            Require(await vm.UnblockAsync("/preview/design"), "Unblock succeeded.");
            Require(!vm.IsSelectedBlocked && vm.CanCompose, "Unblocking restores the composer.");
            vm.SelectSubject(vm.Subjects.First(s => s.IsDefault));
            Require(await vm.SetTimerAsync(604800) && vm.HasTimer && vm.TimerBanner == "Messages disappear after 1 week", "Timer set from the conversation.");
            Require(await vm.SetTimerAsync(null) && !vm.HasTimer, "Timer turned off.");
            Require(!await vm.SetTimerAsync(42), "Only the allowed timers.");
        });
    }

    private sealed class Fixed(System.Net.HttpStatusCode status) : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken token) =>
            Task.FromResult(new HttpResponseMessage(status) { Content = new StringContent("{}") });
    }
}
