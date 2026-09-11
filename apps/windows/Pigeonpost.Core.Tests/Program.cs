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

Test("Apple fixture: received/sent grouping, order and own mailboxes", () =>
{
    Equal(fixtureMessages.Count, 5);
    var rows = Build();
    Equal(string.Join(',', rows.Select(c => c.Peer)), "/k/eeee5555ffff6666gggg7777hh,/bekir/agent1,/bekir/docdex");
    var agent = rows.Single(c => c.Peer == "/bekir/agent1");
    Equal(string.Join(',', agent.Messages.Select(m => m.Id)), "m1,m_out1,m2");
    Equal(agent.Unread, 1); Equal(agent.Held, 1); Equal(agent.Name, "my fleet");
    Equal(rows.Single(c => c.IsMine).Name, "docdex");
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
    public int SendCount { get; private set; }
    public Task<IReadOnlyList<Mailbox>> GetMailboxesAsync(CancellationToken token) => inner.GetMailboxesAsync(token);
    public async Task<InboxSnapshot> LoadAsync(string identity, CancellationToken token)
    {
        if (BeforeLoad is { } before) await before(identity, token);
        // Deliberately ignore cancellation: view-state guards must also handle a late response.
        return await inner.LoadAsync(identity, CancellationToken.None);
    }
    public Task<SendReceipt> SendAsync(string identity, string peer, string body, string? thread, CancellationToken token)
    {
        SendCount++;
        return FailSend ? Task.FromException<SendReceipt>(new HttpRequestException("uncertain delivery")) : inner.SendAsync(identity, peer, body, thread, token);
    }
    public Task<string> CreateThreadAsync(string identity, string peer, string title, CancellationToken token) => inner.CreateThreadAsync(identity, peer, title, token);
    public Task SetArchivedAsync(string identity, string peer, bool archived, CancellationToken token) => inner.SetArchivedAsync(identity, peer, archived, token);
    public Task AcknowledgeAsync(string identity, string id, CancellationToken token) => inner.AcknowledgeAsync(identity, id, token);
}
