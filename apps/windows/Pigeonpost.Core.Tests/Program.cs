using System.Net;
using System.Text.Json;
using Pigeonpost.Core;

// Dependency-free behavioral runner, matching the Apple client's model-test approach.
var tests = new List<(string Name, Func<Task> Run)>();
void Test(string name, Action test) => tests.Add((name, () => { test(); return Task.CompletedTask; }));
void AsyncTest(string name, Func<Task> test) => tests.Add((name, test));
void Check(bool condition, string reason) { if (!condition) throw new Exception(reason); }
void Equal<T>(T actual, T expected) => Check(EqualityComparer<T>.Default.Equals(actual, expected), $"Expected {expected}, got {actual}.");
async Task<T> Throws<T>(Func<Task> action) where T : Exception
{
    try { await action(); } catch (T ex) { return ex; }
    throw new Exception($"Expected {typeof(T).Name}.");
}
string Fixture(string name) => File.ReadAllText(Path.Combine(AppContext.BaseDirectory, "Fixtures", name + ".json"));
var fixtureMessages = JsonSerializer.Deserialize<PostboxClient.InboxResponse>(Fixture("inbox"), PostboxJson.Options)!.Messages!;
var contacts = JsonSerializer.Deserialize<ContactBody>(Fixture("contacts"), PostboxJson.Options)!.Contacts;
var threads = JsonSerializer.Deserialize<ThreadBody>(Fixture("threads"), PostboxJson.Options)!.Threads;
var fixture = new InboxSnapshot(fixtureMessages, threads, contacts, new HashSet<string>());
var acting = new Mailbox("/k/cz6900v2h90vnwefj7g7ezvbh4", "/bekir/su_iam", "su_iam");
Mailbox[] own = [acting, new("/k/zz1111v2h90vnwefj7g7ezvbh9", "/bekir/docdex", "docdex box"), new("/k/qq2222v2h90vnwefj7g7ezvbh7", Label: "scratch")];
IReadOnlyList<Conversation> Build(InboxSnapshot? value = null, IReadOnlyList<PendingMessage>? pending = null) => ConversationBuilder.Build(value ?? fixture, pending ?? [], own, acting);

Test("Default inbox labels distinguish namespaces and preserve routing keys", () =>
{
    Equal(new Mailbox("/k/one", "/bekir/main", "main").DisplayName, "/bekir");
    Equal(new Mailbox("/k/two", "/alp/main", "main").DisplayName, "/alp");
    Equal(new Mailbox("/k/one", "/bekir/main").Key, "/bekir/main");
    Equal(PostAddress.DisplayName("/github/alex/main"), "/github/alex");
    Equal(PostAddress.DisplayName("/ozgur/main"), "/ozgur");
    Equal(PostAddress.DisplayName("/bekir/agent1"), "/bekir/agent1");
    Equal(PostAddress.DisplayName("/wodo/home"), "/wodo/home");
    Equal(PostAddress.DisplayName("/k/abc"), "/k/abc");
});
Test("Conversation entry supplies one slash and accepts namespace and address formats", () =>
{
    Equal(PostAddress.Input(""), "/");
    Equal(PostAddress.Input(" bekir/main "), "/bekir/main");
    Equal(PostAddress.Input("/alp"), "/alp");
    foreach (var address in new[] { "/bekir", "/bekir/main", "/github/alex/main", "/alex+tag@gmail.com", "/alex*tag@gmail.com/main" })
        Check(PostAddress.IsValid(address), "Valid address rejected: " + address);
    foreach (var address in new[] { "/", "/bekir//main", "/bekir/..", "/bekir/*", "/bekir/ma\nin" })
        Check(!PostAddress.IsValid(address), "Invalid address accepted: " + address);
});
Test("Apple fixture: received/sent grouping, order and own mailboxes", () =>
{
    Equal(fixtureMessages.Count, 5);
    var rows = Build();
    Equal(string.Join(',', rows.Select(c => c.Peer)), "/k/eeee5555ffff6666gggg7777hh,/bekir/agent1,/bekir/docdex");
    var agent = rows.Single(c => c.Peer == "/bekir/agent1");
    Equal(string.Join(',', agent.Messages.Select(m => m.Id)), "m1,m_out1,m2");
    Equal(agent.Unread, 1); Equal(agent.Held, 1); Equal(agent.Name, "my fleet");
    Equal(rows.Single(c => c.IsMine).Name, "/bekir/docdex");
    Check(rows.All(c => c.Peer != own[2].Address), "An untouched owned mailbox must not create a conversation.");
});
Test("Repeated server rows do not inflate messages, unread or held counts", () =>
{
    var rows = Build(fixture with { Messages = [.. fixtureMessages, .. fixtureMessages] });
    var agent = rows.Single(c => c.Peer == "/bekir/agent1");
    Equal(agent.Messages.Count, 3); Equal(agent.Unread, 1); Equal(agent.Held, 1);
});
Test("Address and handle versions of a peer merge in both directions", () =>
{
    var sent = fixtureMessages.Single(m => m.IsOutgoing) with { Peer = "/k/aaaa1111bbbb2222cccc3333dd", PeerHandle = null, SenderHandle = acting.Handle };
    var rows = Build(fixture with { Messages = [.. fixtureMessages.Where(m => !m.IsOutgoing), sent] });
    Equal(rows.Single(c => c.Peer == "/bekir/agent1").Messages.Count, 3);
    Check(rows.All(c => c.Peer != acting.Handle), "The sender's own handle must not become the outbound peer.");
});
Test("Pending rows are mailbox-scoped and reconcile against sent_copy_id", () =>
{
    PendingMessage[] pending =
    [
        new("local-1", acting.Address, "/k/aaaa1111bbbb2222cccc3333dd", "local", 1789113600, "t-agent1"),
        new("local-2", "/another/mailbox", "/other/peer", "private draft", 1789113601, null),
        new("local-3", acting.Address, "/bekir/agent1", "already returned", 1789113602, "t-agent1", SentCopyId: "m_out1")
    ];
    var rows = Build(pending: pending);
    Equal(rows.Single(c => c.Peer == "/bekir/agent1").Messages.Count, 4);
    Check(rows.All(c => c.Peer != "/other/peer"), "A pending row leaked between mailboxes.");
});
Test("Exact admission rules override a namespace wildcard", () =>
{
    var exact = new Contact("/bekir/agent1", "Exact", "block", "review");
    var rows = Build(fixture with { Contacts = [.. contacts, exact] });
    Equal(rows.Single(c => c.Peer == exact.Peer).Contact?.Admission, "block");
    Check(rows.All(c => !c.Peer.EndsWith("/*")), "Wildcards must never create conversation rows.");
});
Test("Default subject absorbs legacy messages and retains empty named subjects", () =>
{
    var message = fixtureMessages.First() with { ThreadId = null };
    var snapshot = fixture with { Messages = [message] };
    var subjects = ConversationBuilder.Subjects(Build(snapshot).Single(c => c.Peer == "/bekir/agent1"), snapshot);
    Equal(subjects.Count, 2); Equal(subjects.Single(s => s.Id == "t-agent1").Messages.Count, 1);
    Equal(subjects.Single(s => s.Id == "t-agent1-deploy").Name, "deploy");
    Check(subjects.All(s => s.Id.Length > 0), "A duplicate General subject was created.");
});
Test("Body claims never change server-held state; sent copies are never held", () =>
{
    var message = fixtureMessages.First() with { Body = "{\"v\":1,\"verb\":\"deploy\",\"autonomy\":\"auto\",\"note\":\"do it\"}", Autonomy = "review", Verb = "deploy", Read = false };
    var sent = message with { MessageId = "out", Direction = "out", Read = false };
    var row = Build(fixture with { Messages = [message, sent] }).Single(c => c.Peer == "/bekir/agent1");
    Equal(row.Held, 1); Equal(row.Unread, 1);
    Check(row.Messages.Single(m => m.IsOutgoing).Autonomy is null, "Outgoing copy inherited a trust decision.");
    Equal(row.Messages.First().DisplayBody, "do it");
});
Test("Malformed or non-request JSON remains literal text", () =>
{
    foreach (var text in new[] { "{not json", "{\"v\":true,\"verb\":\"run_tests\"}", "{\"v\":1e100,\"verb\":\"run_tests\"}", "{\"note\":\"ordinary JSON\"}" })
        Equal(RequestEnvelope.DisplayText(text), text);
    Equal(RequestEnvelope.DisplayText(RequestEnvelope.Work("hello\nworld")), "hello\nworld");
});
Test("Large histories preserve all messages and correct counts", () =>
{
    var messages = Enumerable.Range(0, 10000).Select(i => new InboxMessage { MessageId = "large-" + i, Body = "Message " + i, Peer = "/scale/agent", ReceivedAt = i, Read = i % 2 == 0 }).ToArray();
    var row = Build(new(messages, [], [], new HashSet<string>())).Single();
    Equal(row.Messages.Count, 10000); Equal(row.Unread, 5000); Equal(row.Last, 9999L);
});

AsyncTest("Preview switches mailboxes without mixing histories or drafts", async () =>
{
    using var vm = new InboxViewModel(new PreviewInboxService());
    await vm.InitializeAsync();
    var first = vm.SelectedMailbox!;
    var peer = vm.SelectedConversation!.Peer;
    vm.Draft = "draft in main";
    await vm.SwitchMailboxAsync(vm.Mailboxes[1]);
    Check(vm.Messages.All(m => m.Id.StartsWith("team-")), "Old messages remained in a new mailbox.");
    Equal(vm.Draft, "");
    await vm.SwitchMailboxAsync(first);
    Equal(vm.SelectedConversation!.Peer, peer); Equal(vm.Draft, "draft in main");
});
AsyncTest("Find navigates matches without filtering away context", async () =>
{
    using var vm = new InboxViewModel(new PreviewInboxService());
    await vm.InitializeAsync();
    var count = vm.Messages.Count;
    vm.Find = "desktop";
    Check(vm.CurrentMatch is not null, "Expected a find result.");
    Equal(vm.Messages.Count, count);
    var first = vm.CurrentMatch!.Id;
    vm.MoveMatch(-1); vm.MoveMatch(1);
    Equal(vm.CurrentMatch!.Id, first);
    vm.Find = "missing phrase"; Equal(vm.FindSummary, "No matches");
});
AsyncTest("Subjects retain separate drafts and sends reconcile once", async () =>
{
    using var vm = new InboxViewModel(new PreviewInboxService());
    await vm.InitializeAsync();
    vm.Draft = "general draft";
    var original = vm.SelectedSubject!.Id;
    await vm.CreateSubjectAsync("A new topic");
    Equal(vm.SubjectTitle, "A new topic"); Equal(vm.Draft, "");
    vm.Draft = "hello from Windows";
    await vm.SendDraftAsync();
    Equal(vm.Draft, ""); Equal(vm.Messages.Count(m => m.DisplayBody == "hello from Windows"), 1);
    vm.SelectSubject(vm.Subjects.Single(s => s.Id == original));
    Equal(vm.Draft, "general draft");
});
AsyncTest("Archive is reversible and acknowledgements retain held status", async () =>
{
    using var vm = new InboxViewModel(new PreviewInboxService());
    await vm.InitializeAsync();
    vm.SelectConversation(vm.Conversations.Single(c => c.Peer == "/preview/engineering"));
    await vm.AcknowledgeSelectedAsync();
    Equal(vm.SelectedConversation!.Unread, 0); Equal(vm.SelectedConversation.Held, 1);
    await vm.ArchiveSelectedAsync();
    Check(vm.Conversations.All(c => c.Peer != "/preview/engineering"), "Archive did not hide the conversation.");
    vm.ShowArchived = true; Equal(vm.SelectedConversation!.Peer, "/preview/engineering");
    await vm.ArchiveSelectedAsync(); vm.ShowArchived = false;
    Check(vm.Conversations.Any(c => c.Peer == "/preview/engineering"), "Restore lost the conversation.");
});
AsyncTest("New conversation has its first message and address validation", async () =>
{
    using var vm = new InboxViewModel(new PreviewInboxService());
    await vm.InitializeAsync();
    await vm.StartConversationAsync("/preview/new-agent", "First message");
    Equal(vm.SelectedConversation!.Peer, "/preview/new-agent"); Equal(vm.Messages.Single().DisplayBody, "First message");
    await vm.StartConversationAsync("/preview/*", "invalid target"); Check(vm.HasError, "Wildcard conversation accepted.");
});
AsyncTest("Failed send preserves the draft and makes no automatic retry", async () =>
{
    var service = new WrappedService { FailSend = true };
    using var vm = new InboxViewModel(service);
    await vm.InitializeAsync(); vm.Draft = "keep this draft";
    await vm.SendDraftAsync();
    Equal(service.SendCount, 1); Equal(vm.Draft, "keep this draft"); Check(vm.HasError, "Send failure was hidden.");
    Equal(vm.Messages.Last().StatusLabel, "Delivery not confirmed");
});
AsyncTest("A stale mailbox load cannot overwrite the new mailbox", async () =>
{
    var service = new WrappedService();
    var firstStarted = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
    var releaseFirst = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
    service.BeforeLoad = async (identity, _) => { if (identity == "/k/preview-main") { firstStarted.TrySetResult(); await releaseFirst.Task; } };
    using var vm = new InboxViewModel(service);
    var initial = vm.InitializeAsync();
    await firstStarted.Task;
    await vm.SwitchMailboxAsync(vm.Mailboxes[1]);
    releaseFirst.SetResult(); await initial;
    Equal(vm.SelectedMailbox!.Address, "/k/preview-team");
    Check(vm.Messages.All(m => m.Id.StartsWith("team-")), "A cancelled response replaced the active mailbox.");
});

AsyncTest("Inbox query keeps sent/read history, wait and escaped identity", async () =>
{
    var handler = new Handler((request, _) =>
    {
        var query = request.RequestUri!.Query;
        Check(query.Contains("include_sent=true") && query.Contains("include_read=true") && query.Contains("wait=25"), "Missing history or long-poll flags.");
        Check(query.Contains("%3F") && query.Contains("%26"), "Identity query was not escaped.");
        Equal(request.Headers.Authorization?.ToString(), "Bearer old");
        return Task.FromResult(Json(HttpStatusCode.OK, Fixture("inbox")));
    });
    using var http = new HttpClient(handler); using var client = new PostboxClient(http, new Tokens());
    Equal((await client.GetInboxAsync("/wodo/test?x=1&z=2", 25)).Messages!.Count, 5);
});
AsyncTest("Send uses the Apple wire contract and exposes sent_copy_id", async () =>
{
    var handler = new Handler(async (request, token) =>
    {
        Equal(request.Method, HttpMethod.Post); Equal(request.RequestUri!.AbsolutePath, "/v1/send");
        using var json = JsonDocument.Parse(await request.Content!.ReadAsStringAsync(token));
        var root = json.RootElement;
        Equal(root.GetProperty("from").GetString(), "/k/main"); Equal(root.GetProperty("to").GetString(), "/wodo/agent");
        Equal(root.GetProperty("thread_id").GetString(), "subject-1"); Equal(root.GetProperty("body").GetString(), "hello");
        return Json(HttpStatusCode.OK, "{\"message_id\":\"received\",\"sent_copy_id\":\"sent\"}");
    });
    using var http = new HttpClient(handler); using var client = new PostboxClient(http, new Tokens());
    Equal((await client.SendAsync("/k/main", "/wodo/agent", "hello", "subject-1", default)).SentCopyId, "sent");
});
AsyncTest("One 401 renews once and retries with the new token", async () =>
{
    var tokens = new Tokens(); var calls = 0;
    var handler = new Handler((request, _) => Task.FromResult(++calls == 1 ? Json(HttpStatusCode.Unauthorized, "{}") : Json(HttpStatusCode.OK, "{\"messages\":[]}")));
    using var http = new HttpClient(handler); using var client = new PostboxClient(http, tokens);
    await client.GetInboxAsync("/k/main"); Equal(calls, 2); Equal(tokens.Renewals, 1);
});
AsyncTest("Concurrent unauthorized requests share one rotating-token renewal", async () =>
{
    var tokens = new Tokens(); var firstCalls = 0;
    var both = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
    var handler = new Handler(async (request, cancellationToken) =>
    {
        if (request.Headers.Authorization?.Parameter == "old")
        {
            if (Interlocked.Increment(ref firstCalls) == 2) both.TrySetResult();
            await both.Task.WaitAsync(cancellationToken);
            return Json(HttpStatusCode.Unauthorized, "{}");
        }
        return Json(HttpStatusCode.OK, "{\"messages\":[]}");
    });
    using var http = new HttpClient(handler); using var client = new PostboxClient(http, tokens);
    await Task.WhenAll(client.GetInboxAsync("/k/one"), client.GetInboxAsync("/k/two"));
    Equal(firstCalls, 2); Equal(tokens.Renewals, 1);
});
AsyncTest("Repeated 401 terminates after the bounded retry", async () =>
{
    var calls = 0; var tokens = new Tokens();
    var handler = new Handler((_, _) => { calls++; return Task.FromResult(Json(HttpStatusCode.Unauthorized, "{}")); });
    using var http = new HttpClient(handler); using var client = new PostboxClient(http, tokens);
    Equal((await Throws<PostboxException>(() => client.GetInboxAsync("/k/main"))).Code, "unauthorized");
    Equal(calls, 2); Equal(tokens.Renewals, 1);
});
AsyncTest("Rejected sends translate server codes without retrying", async () =>
{
    var calls = 0;
    var handler = new Handler((_, _) => { calls++; return Task.FromResult(Json(HttpStatusCode.Forbidden, "{\"error\":\"not_admitted\",\"detail\":\"internal details\"}")); });
    using var http = new HttpClient(handler); using var client = new PostboxClient(http, new Tokens());
    var error = await Throws<PostboxException>(() => client.SendAsync("/k/main", "/wodo/agent", "hello", null, default));
    Equal(error.Message, "They are not accepting mail from this mailbox."); Equal(calls, 1);
});
AsyncTest("An uncertain POST transport failure is never automatically resent", async () =>
{
    var calls = 0;
    var handler = new Handler((_, _) => { calls++; throw new HttpRequestException("disconnected after upload"); });
    using var http = new HttpClient(handler); using var client = new PostboxClient(http, new Tokens());
    await Throws<HttpRequestException>(() => client.SendAsync("/k/main", "/wodo/agent", "hello", null, default));
    Equal(calls, 1);
});
AsyncTest("Long-poll cancellation reaches the HTTP transport", async () =>
{
    var started = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
    var handler = new Handler(async (_, token) => { started.SetResult(); await Task.Delay(Timeout.Infinite, token); return Json(HttpStatusCode.OK, "{}"); });
    using var http = new HttpClient(handler); using var client = new PostboxClient(http, new Tokens());
    using var cancellation = new CancellationTokenSource();
    var poll = client.GetInboxAsync("/k/main", 25, cancellation.Token);
    await started.Task; cancellation.Cancel(); await Throws<OperationCanceledException>(() => poll);
});
AsyncTest("Unknown fields decode while malformed or null required data fails", async () =>
{
    foreach (var body in new[] { "not json", "{\"messages\":[{\"body\":\"missing id\"}]}", "{\"messages\":[{\"message_id\":\"1\",\"body\":null}]}" })
    {
        using var http = new HttpClient(new Handler((_, _) => Task.FromResult(Json(HttpStatusCode.OK, body))));
        using var client = new PostboxClient(http, new Tokens());
        Equal((await Throws<PostboxException>(() => client.GetInboxAsync("/k/main"))).Code, "bad_response");
    }
    using var oldHttp = new HttpClient(new Handler((_, _) => Task.FromResult(Json(HttpStatusCode.OK, "{\"messages\":[{\"message_id\":\"old\",\"body\":\"hello\",\"from\":\"/old/peer\",\"future_field\":true}]}"))));
    using var oldClient = new PostboxClient(oldHttp, new Tokens());
    var message = (await oldClient.GetInboxAsync("/k/main")).Messages!.Single();
    Check(!message.IsOutgoing && message.ThreadId is null, "Legacy defaults changed.");
});
Test("Bearer endpoints require HTTPS outside loopback", () =>
{
    using var http = new HttpClient();
    try { using var client = new PostboxClient(http, new Tokens(), new Uri("http://example.com")); throw new Exception("Plain HTTP accepted."); }
    catch (ArgumentException) { }
});

AsyncTest("Attachments are scoped, bounded, and sent as file identifiers", async () =>
{
    var calls = new List<string>();
    using var http = new HttpClient(new Handler(async (request, ct) =>
    {
        var path = request.RequestUri!.AbsolutePath;
        calls.Add(path);
        if (path == "/v1/attachments")
        {
            Equal(request.Headers.GetValues("x-pigeonpost-identity").Single(), "/k/main");
            Check(!request.Headers.GetValues("x-pigeonpost-filename").Single().Contains('\n'), "Unsafe filename header.");
            Equal((await request.Content!.ReadAsByteArrayAsync(ct)).Length, 3);
            return Json(HttpStatusCode.Created, "{\"id\":\"file-1\",\"filename\":\"note.txt\",\"media_type\":\"application/octet-stream\",\"bytes\":3}");
        }
        if (path == "/v1/send")
        {
            using var body = JsonDocument.Parse(await request.Content!.ReadAsStringAsync(ct));
            Equal(body.RootElement.GetProperty("attachments")[0].GetString(), "file-1");
            Equal(body.RootElement.GetProperty("thread_id").GetString(), "subject");
            return Json(HttpStatusCode.OK, "{\"message_id\":\"m1\",\"sent_copy_id\":\"s1\"}");
        }
        Equal(request.Headers.GetValues("x-pigeonpost-identity").Single(), "/k/main");
        return new HttpResponseMessage(HttpStatusCode.OK) { Content = new ByteArrayContent([1, 2, 3]) };
    }));
    using var client = new PostboxClient(http, new Tokens());
    var uploaded = await client.UploadAsync("/k/main", "note\n.txt", [1, 2, 3], default);
    await client.SendAttachmentAsync("/k/main", "/peer", "hello", "subject", uploaded.Id, default);
    Equal((await client.DownloadAsync("/k/main", uploaded.Id, default)).Length, 3);
    await Throws<ArgumentException>(() => client.UploadAsync("/k/main", "empty", [], default));
    Equal(calls.Count, 3);
});
AsyncTest("Contact writes carry mailbox and explicit review permissions", async () =>
{
    using var http = new HttpClient(new Handler(async (request, ct) =>
    {
        Equal(request.Method, HttpMethod.Put);
        Equal(request.RequestUri!.AbsolutePath, "/v1/contacts");
        using var body = JsonDocument.Parse(await request.Content!.ReadAsStringAsync(ct));
        var root = body.RootElement;
        Equal(root.GetProperty("identity").GetString(), "/k/main");
        Equal(root.GetProperty("admission").GetString(), "block");
        Equal(root.GetProperty("autonomy").GetString(), "review");
        Equal(root.GetProperty("allowed_verbs").GetArrayLength(), 0);
        return Json(HttpStatusCode.OK, "{\"ok\":true}");
    }));
    using var client = new PostboxClient(http, new Tokens());
    await client.SetContactAsync("/k/main", new Contact("/peer", "Peer", "block", "review", []), default);
});
AsyncTest("A first mailbox is created without local identity secrets", async () =>
{
    using var http = new HttpClient(new Handler(async (request, ct) =>
    {
        Equal(request.RequestUri!.AbsolutePath, "/v1/identities");
        Equal(request.Method, HttpMethod.Post);
        Equal(await request.Content!.ReadAsStringAsync(ct), "{}");
        return Json(HttpStatusCode.Created, "{\"address\":\"/k/new\"}");
    }));
    using var client = new PostboxClient(http, new Tokens());
    Equal(await client.CreateMailboxAsync(default), "/k/new");
});
AsyncTest("Open-only keeps a local conversation without sending or granting trust", async () =>
{
    var service = new WrappedService();
    using var vm = new InboxViewModel(service);
    await vm.InitializeAsync();
    await vm.StartConversationAsync("new-person", "");
    Equal(vm.SelectedConversation!.Peer, "/new-person/main");
    Equal(service.SendCount, 0); Equal(vm.Messages.Count, 0);
    Check(vm.SelectedConversation.Contact is null, "Opening granted known-sender status.");
    await vm.RefreshAsync(); Equal(vm.SelectedConversation.Peer, "/new-person/main");
    vm.Draft = "local draft";
    var main = vm.SelectedMailbox!;
    await vm.SwitchMailboxAsync(vm.Mailboxes[1]);
    Check(vm.Conversations.All(c => c.Peer != "/new-person/main"), "Local conversation leaked to another mailbox.");
    await vm.SwitchMailboxAsync(main);
    Equal(vm.SelectedConversation!.Peer, "/new-person/main"); Equal(vm.Draft, "local draft");
});
AsyncTest("A failed first message remains selected with its draft and delivery state", async () =>
{
    var service = new WrappedService { FailSend = true };
    using var vm = new InboxViewModel(service); await vm.InitializeAsync();
    await vm.StartConversationAsync("/new-person/main", "keep my first message");
    Equal(vm.SelectedConversation!.Peer, "/new-person/main"); Equal(vm.Draft, "keep my first message");
    Equal(vm.Messages.Single().Status, DeliveryStatus.Failed); Equal(service.SendCount, 1);
    await vm.RefreshAsync(); Equal(vm.Draft, "keep my first message"); Equal(service.SendCount, 1);
});
AsyncTest("Opening an existing draft does not send it without review", async () =>
{
    var service = new WrappedService(); using var vm = new InboxViewModel(service); await vm.InitializeAsync();
    var peer = vm.SelectedConversation!.Peer; vm.Draft = "saved draft";
    await vm.StartConversationAsync(peer, "new text");
    Equal(service.SendCount, 0); Check(vm.Draft.Contains("saved draft") && vm.Draft.Contains("new text"), "Draft text lost.");
    Check(vm.HasError, "Combined draft needs review.");
});
AsyncTest("Empty subjects and owned addresses remain discoverable", async () =>
{
    using var vm = new InboxViewModel(new PreviewInboxService()); await vm.InitializeAsync();
    await vm.StartConversationAsync("/k/preview-team", "");
    Equal(vm.SelectedConversation!.Peer, "/preview/team"); Check(vm.SelectedConversation.IsMine, "Own mailbox not recognized.");
    await vm.CreateSubjectAsync("Empty subject");
    Equal(vm.SubjectTitle, "Empty subject"); Equal(vm.Messages.Count, 0);
    var rows = ConversationBuilder.Build(new([], [new("empty", "/some/agent", "Empty")], [], new HashSet<string>()), [], [], new("/k/main"));
    Equal(rows.Single().Peer, "/some/agent");
});
AsyncTest("Late first-send completion cannot clear or replace another mailbox draft", async () =>
{
    var started = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
    var release = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
    var service = new WrappedService { BeforeSend = async () => { started.SetResult(); await release.Task; } };
    using var vm = new InboxViewModel(service); await vm.InitializeAsync();
    var sending = vm.StartConversationAsync("/new/person", "first body"); await started.Task;
    await vm.SwitchMailboxAsync(vm.Mailboxes[1]); vm.Draft = "team draft";
    release.SetResult(); await sending;
    Equal(vm.SelectedMailbox!.Key, "/preview/team"); Equal(vm.Draft, "team draft");
    Check(vm.Messages.All(m => m.Id.StartsWith("team-")), "Late send appeared in the wrong mailbox.");
});
AsyncTest("Repeated switching preserves each selected subject, draft and context generation", async () =>
{
    using var vm = new InboxViewModel(new PreviewInboxService()); await vm.InitializeAsync();
    var main = vm.Mailboxes[0]; var team = vm.Mailboxes[1];
    vm.SelectSubject(vm.Subjects.Single(s => s.Id == "design-release")); vm.Draft = "release draft";
    var version = vm.ContextVersion;
    for (var i = 0; i < 6; i++)
    {
        await vm.SwitchMailboxAsync(team); vm.Draft = "team draft";
        Check(vm.ContextVersion > version, "Context did not invalidate deferred UI work."); version = vm.ContextVersion;
        await vm.SwitchMailboxAsync(main); Equal(vm.SelectedSubject!.Id, "design-release"); Equal(vm.Draft, "release draft");
        Check(vm.Messages.All(m => m.Id == "release-1"), "Subject crossed mailboxes.");
    }
});
AsyncTest("Long-history find retains context and its selected match across refresh", async () =>
{
    using var vm = new InboxViewModel(new PreviewInboxService(longHistory: true)); await vm.InitializeAsync();
    Equal(vm.Messages.Count, 38);
    vm.Find = "History message"; Equal(vm.CurrentMatch!.Id, "history-1");
    Equal(vm.FindSummary, "1 of 35"); vm.MoveMatch(7); Equal(vm.CurrentMatch!.Id, "history-8");
    var notifications = 0; vm.PropertyChanged += (_, e) => { if (e.PropertyName == nameof(vm.CurrentMatch)) notifications++; };
    await vm.RefreshAsync(background: true);
    Equal(vm.CurrentMatch!.Id, "history-8"); Equal(notifications, 0);
    vm.SelectSubject(vm.Subjects.Single(s => s.Id == "design-release")); Equal(vm.Messages.Count, 1);
    vm.SelectSubject(vm.Subjects.Single(s => s.Id == "design-general"));
    Equal(vm.Find, ""); Equal(vm.Messages.Count, 38);
});
AsyncTest("Known sender does not grant requests; full grants exclude forbidden and unknown verbs", async () =>
{
    using var vm = new InboxViewModel(new PreviewInboxService()); await vm.InitializeAsync();
    var peer = vm.SelectedConversation!.Peer;
    await vm.SaveContactAsync(peer, true, "Design", false, []);
    Equal(vm.SelectedConversation!.Contact!.Autonomy, "review");
    await vm.SaveContactAsync(peer, true, "Design", false, ["run_tests", "read_file", "deploy", "invented"]);
    Equal(vm.SelectedConversation!.Contact!.Autonomy, "auto");
    Check(vm.SelectedConversation.Contact.AllowedVerbs!.SequenceEqual(new[] { "run_tests", "read_file" }), "Unexpected request permission granted.");
    await vm.SaveContactAsync(peer, true, "Design", true, vm.GrantableVerbs);
    Equal(vm.SelectedConversation!.Contact!.Admission, "block"); Equal(vm.SelectedConversation.Contact.Autonomy, "review");
    Equal(vm.SelectedConversation.Contact.AllowedVerbs!.Count, 0);
    await vm.SaveContactAsync(peer, true, "Design", false, []);
    Equal(vm.SelectedConversation!.Contact!.Autonomy, "review");
});
AsyncTest("Removing an exact contact reverts to namespace policy", async () =>
{
    var service = new PreviewInboxService();
    await service.SetContactAsync("/k/preview-main", new("/preview/*", "Fleet", "allow", "review", []), default);
    using var vm = new InboxViewModel(service); await vm.InitializeAsync();
    var peer = vm.SelectedConversation!.Peer;
    await vm.SaveContactAsync(peer, false, null, false, []);
    Equal(vm.SelectedConversation!.Contact!.Peer, "/preview/*");
    Check(vm.ExactContact(peer) is null, "Exact contact not removed.");
});
AsyncTest("Deleting a subject removes only its messages and draft in the active mailbox", async () =>
{
    using var vm = new InboxViewModel(new PreviewInboxService()); await vm.InitializeAsync();
    vm.SelectSubject(vm.Subjects.Single(s => s.Id == "design-release")); vm.Draft = "discard with subject";
    await vm.DeleteSubjectAsync();
    Check(vm.Subjects.All(s => s.Id != "design-release"), "Deleted subject retained.");
    Check(vm.SelectedConversation!.Messages.All(m => m.Id != "release-1"), "Deleted mail retained.");
    Equal(vm.Messages.Count, 3); Equal(vm.Draft, "");
    await vm.SwitchMailboxAsync(vm.Mailboxes[1]); Equal(vm.Messages.Single().Id, "team-1");
});
AsyncTest("Contact removal and subject deletion use scoped Apple API contracts", async () =>
{
    var calls = 0;
    using var http = new HttpClient(new Handler(async (request, ct) =>
    {
        calls++;
        if (request.Method == HttpMethod.Put)
        {
            using var json = JsonDocument.Parse(await request.Content!.ReadAsStringAsync(ct));
            Equal(request.RequestUri!.AbsolutePath, "/v1/contacts");
            Equal(json.RootElement.GetProperty("identity").GetString(), "/k/test");
            Equal(json.RootElement.GetProperty("peer").GetString(), "/peer/main");
            Check(json.RootElement.GetProperty("remove").GetBoolean(), "Missing removal flag.");
        }
        else
        {
            Equal(request.Method, HttpMethod.Delete);
            Check(request.RequestUri!.AbsolutePath.Contains("thread%2Fid"), "Thread identifier not escaped.");
            Check(request.RequestUri.Query.Contains("%2Fk%2Ftest"), "Mailbox not scoped.");
            Check(request.Content is null, "DELETE should not carry a JSON body.");
        }
        return Json(HttpStatusCode.OK, "{}");
    }));
    using var client = new PostboxClient(http, new Tokens());
    await client.RemoveContactAsync("/k/test", "/peer/main", default);
    await client.DeleteThreadAsync("/k/test", "thread/id", default); Equal(calls, 2);
});
AsyncTest("Contact vocabulary is decoded and conflicting forbidden verbs cannot be offered", async () =>
{
    using var http = new HttpClient(new Handler((request, _) => Task.FromResult(Json(HttpStatusCode.OK,
        request.RequestUri!.AbsolutePath == "/v1/contacts" ? "{\"contacts\":[],\"vocabulary\":{\"grantable\":[\"read_file\",\"deploy\"],\"never_auto\":[\"deploy\"]}}" : "{}"))));
    using var client = new PostboxClient(http, new Tokens());
    var snapshot = await client.LoadAsync("/k/test", default);
    Equal(snapshot.Vocabulary!.SafeGrants.Single(), "read_file");
});
tests.AddRange(AccountTests.All());
var failed = 0;
foreach (var test in tests)
{
    try { await test.Run().WaitAsync(TimeSpan.FromSeconds(10)); Console.WriteLine("PASS " + test.Name); }
    catch (Exception ex) { failed++; Console.Error.WriteLine("FAIL " + test.Name + ": " + ex.Message); }
}
Console.WriteLine($"{tests.Count - failed}/{tests.Count} tests passed.");
return failed == 0 ? 0 : 1;

static HttpResponseMessage Json(HttpStatusCode status, string body) => new(status) { Content = new StringContent(body, System.Text.Encoding.UTF8, "application/json") };
sealed record ContactBody(IReadOnlyList<Contact> Contacts);
sealed record ThreadBody(IReadOnlyList<ServerThread> Threads);
sealed class Handler(Func<HttpRequestMessage, CancellationToken, Task<HttpResponseMessage>> handle) : HttpMessageHandler
{
    protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken) => handle(request, cancellationToken);
}
sealed class Tokens : IAccessTokenProvider
{
    private string token = "old";
    public int Renewals { get; private set; }
    public Task<string> GetTokenAsync(CancellationToken cancellationToken) => Task.FromResult(token);
    public Task<string> RefreshTokenAsync(CancellationToken cancellationToken) { Renewals++; token = "new"; return Task.FromResult(token); }
}
sealed class WrappedService : IInboxService
{
    private readonly PreviewInboxService inner = new();
    public Func<string, CancellationToken, Task>? BeforeLoad { get; set; }
    public bool FailSend { get; init; }
    public Func<Task>? BeforeSend { get; init; }
    public int SendCount { get; private set; }
    public Task<IReadOnlyList<Mailbox>> GetMailboxesAsync(CancellationToken token) => inner.GetMailboxesAsync(token);
    public async Task<InboxSnapshot> LoadAsync(string identity, CancellationToken token)
    {
        if (BeforeLoad is { } before) await before(identity, token);
        // Deliberately ignore cancellation: view-state guards must also handle a late response.
        return await inner.LoadAsync(identity, CancellationToken.None);
    }
    public async Task<SendReceipt> SendAsync(string identity, string peer, string body, string? thread, CancellationToken token)
    {
        SendCount++;
        if (BeforeSend is { } before) await before();
        if (FailSend) throw new HttpRequestException("uncertain delivery");
        return await inner.SendAsync(identity, peer, body, thread, CancellationToken.None);
    }
    public Task<string> CreateThreadAsync(string identity, string peer, string title, CancellationToken token) => inner.CreateThreadAsync(identity, peer, title, token);
    public Task SetArchivedAsync(string identity, string peer, bool archived, CancellationToken token) => inner.SetArchivedAsync(identity, peer, archived, token);
    public Task AcknowledgeAsync(string identity, string id, CancellationToken token) => inner.AcknowledgeAsync(identity, id, token);
    public Task SetContactAsync(string identity, Contact contact, CancellationToken token) => inner.SetContactAsync(identity, contact, token);
    public Task RemoveContactAsync(string identity, string peer, CancellationToken token) => inner.RemoveContactAsync(identity, peer, token);
    public Task DeleteThreadAsync(string identity, string id, CancellationToken token) => inner.DeleteThreadAsync(identity, id, token);
}
