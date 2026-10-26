using System.Net;
using System.Text;
using System.Text.Json;
using Pigeonpost.Core;

internal static class MessageActionsTests
{
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Client message references carry 128 random bits and canonical base32 padding", () =>
        {
            var refs = Enumerable.Range(0, 256).Select(_ => MessageReferences.New()).ToArray();
            Require(refs.Distinct().Count() == refs.Length && refs.All(r => MessageReferences.IsValid(r) && "aeimquy4".Contains(r[^1])));
            Require(!MessageReferences.IsValid("mref_ABC") && !MessageReferences.IsValid(new string('a', 31)));
            return Task.CompletedTask;
        });
        yield return ("Action requests preserve immutable file IDs and count Unicode quote characters", () =>
        {
            var ids = new List<string> { "file_first" };
            var request = new MessageSendRequest(Own, Peer, "fixture body", attachments: ids, replyTo: "parent_id", quote: string.Concat(Enumerable.Repeat("😀", 200)));
            var before = JsonSerializer.Serialize(request, PostboxJson.Options); ids[0] = "file_replacement";
            Require(JsonSerializer.Serialize(request, PostboxJson.Options) == before && request.Attachments!.Single() == "file_first");
            Require(request.ToString() == "MessageSendRequest(redacted)");
            Reject(() => new MessageSendRequest(Own, Peer, "x", replyTo: "parent", editOf: "root"));
            Reject(() => new MessageSendRequest(Own, Peer, "x", quote: "orphan"));
            Reject(() => new MessageSendRequest(Own, Peer, "x", replyTo: "parent", quote: new string('x', 201)));
            Reject(() => new MessageSendRequest(Own, Peer, "x", attachments: ids, editOf: "root"));
            return Task.CompletedTask;
        });
        yield return ("Lost direct action responses are never automatically replayed and explicit retries retain exact bytes", async () =>
        {
            var bodies = new List<string>();
            using var f = new Fixture(async request =>
            {
                Require(request.RequestUri!.AbsolutePath == "/v1/send");
                bodies.Add(await request.Content!.ReadAsStringAsync());
                if (bodies.Count == 1) throw new IOException("Synthetic response loss.");
                return Json(new { message_id = "recipient_copy", sent_copy_id = "sender_copy", msg_ref = Ref, duplicate = true });
            });
            var action = new MessageSendRequest(Own, Peer, "fixture revised text", msgRef: Ref, editOf: "root_copy");
            try { await f.Client.SendMessageAsync(action, default); throw new Exception("Expected response loss."); } catch (IOException) { }
            Require(bodies.Count == 1);
            var recovered = await f.Client.SendMessageAsync(action, default);
            Require(bodies.Count == 2 && bodies[0] == bodies[1] && recovered.Duplicate == true && recovered.MsgRef == Ref);
        });
        yield return ("Human inbox reads request folded actions and acknowledge every version", async () =>
        {
            var reads = 0; var acknowledgements = 0;
            using var f = new Fixture(async request =>
            {
                if (request.Method == HttpMethod.Get)
                {
                    var query = System.Web.HttpUtility.ParseQueryString(request.RequestUri!.Query);
                    Require(query["actions"] == "1" && query["fold"] == "1" && query["identity"] == Own);
                    reads++; return Json(new { messages = Array.Empty<object>() });
                }
                using var body = JsonDocument.Parse(await request.Content!.ReadAsStringAsync());
                Require(body.RootElement.GetProperty("with_edits").GetBoolean() && body.RootElement.GetProperty("message_id").GetString() == "root_copy");
                acknowledgements++; return Json(new { });
            });
            await f.Client.GetInboxAsync(Own); await f.Client.GetFilteredAsync(Own, default);
            await f.Client.AcknowledgeAsync(Own, "root_copy", default);
            Require(reads == 2 && acknowledgements == 1);
        });
        yield return ("Folded native rows retain action metadata and reconciliation notices new receipts", () =>
        {
            var source = new InboxMessage { MessageId = "root_copy", Body = "Latest text", Direction = "out", To = Peer, SentAt = 100,
                MsgRef = Ref, ReplyTo = new(MessageId: "parent_copy", Quote: "A remembered quote"), EditedAt = 120, EditCount = 2,
                Receipt = new("delivered", 110), AgentStatus = new("working", 115) };
            var row = Build(source);
            Require(row.MsgRef == Ref && row.ReplyTo?.MessageId == "parent_copy" && row.EditedLabel == "Edited" && row.ReceiptLabel == "Working");
            var seen = Build(source with { Receipt = new("seen", 110, 130), AgentStatus = null });
            Require(seen.ReceiptLabel == "Seen" && !StableList.SameMessage(row, seen));
            var incoming = Build(source with { Direction = "in", From = Peer });
            Require(incoming.Receipt is null && incoming.AgentStatus is null && incoming.ReceiptLabel == "");
            Require((row with { Receipt = new("unknown"), AgentStatus = new("done") }).ReceiptLabel == "");
            return Task.CompletedTask;
        });
        yield return ("Native edit eligibility observes capability, direction, pending state, expiry and the original window", () =>
        {
            var enabled = new ServerFeatures(new HashSet<string> { ServerFeatures.MessageEdit });
            var row = new ThreadMessage("root_copy", "Text", 100, null, true);
            Require(row.CanEdit(enabled, 100) && row.CanEdit(enabled, 86500) && !row.CanEdit(enabled, 86501));
            Require(!row.CanEdit(ServerFeatures.None, 100) && !row.CanEdit(enabled, 99));
            Require(!(row with { IsOutgoing = false }).CanEdit(enabled, 100) && !(row with { System = true }).CanEdit(enabled, 100));
            Require(!(row with { Status = DeliveryStatus.Sending }).CanEdit(enabled, 100) && !(row with { ExpiresAt = 100 }).CanEdit(enabled, 100));
            return Task.CompletedTask;
        });
        yield return ("Repeated polling preserves a quoted row with unchanged structured arguments", () =>
        {
            const string reply = "{\"message_id\":\"parent_copy\",\"quote\":\"Fixture\",\"verb\":\"answer_question\",\"args\":{\"question\":\"Fixture question\"}}";
            var row = new ThreadMessage("copy", "Fixture answer", 100, null, false,
                ReplyTo: JsonSerializer.Deserialize<MessageReply>(reply, PostboxJson.Options));
            var same = row with { ReplyTo = JsonSerializer.Deserialize<MessageReply>(reply, PostboxJson.Options) };
            Require(StableList.SameMessage(row, same));
            var changed = row with { ReplyTo = JsonSerializer.Deserialize<MessageReply>(reply.Replace("Fixture question", "Different question"), PostboxJson.Options) };
            Require(!StableList.SameMessage(row, changed));
            return Task.CompletedTask;
        });
        yield return ("Info and deletion target the owned copy and malformed IDs acquire no token", async () =>
        {
            var calls = 0;
            using var f = new Fixture(async request =>
            {
                calls++; Require(request.Headers.GetValues("x-pigeonpost-identity").Single() == Own);
                if (request.Method == HttpMethod.Get)
                {
                    Require(request.RequestUri!.AbsolutePath == "/v1/messages/root_copy/info");
                    return Json(new { message_id = "root_copy", versions = new[] { new { message_id = "root_copy", body = "Original", at = 100 }, new { message_id = "edit_copy", body = "Revised", at = 120 } } });
                }
                using var body = JsonDocument.Parse(await request.Content!.ReadAsStringAsync());
                Require(request.RequestUri!.AbsolutePath == "/v1/messages/delete" && body.RootElement.GetProperty("message_id").GetString() == "root_copy");
                return Json(new { });
            });
            var info = await f.Client.GetMessageInfoAsync(Own, "root_copy", default);
            Require(info.Versions?.Count == 2 && info.Versions[1].Body == "Revised");
            await f.Client.DeleteMessageAsync(Own, "root_copy", default);
            var tokens = f.Tokens.Acquisitions;
            await Invalid(() => f.Client.GetMessageInfoAsync(Own, "../foreign", default));
            await Invalid(() => f.Client.DeleteMessageAsync(Own, "invalid/id", default));
            Require(calls == 2 && f.Tokens.Acquisitions == tokens);
        });
        yield return ("A new action route rejects a response after its accepted grant is replaced", async () =>
        {
            Fixture? fixture = null;
            using var f = fixture = new Fixture(_ =>
            {
                fixture!.Tokens.Root.Retire(); fixture.Tokens.Root = new AuthorizationLease();
                return Task.FromResult(Json(new { message_id = "root_copy", versions = Array.Empty<object>() }));
            });
            try { await f.Client.GetMessageInfoAsync(Own, "root_copy", default); throw new Exception("Expected retired authorization."); }
            catch (AuthorizationRetiredException) { }
            Require(f.Tokens.Acquisitions == 1);
        });
        yield return ("Read receipt privacy is typed without altering unknown policy fields", () =>
        {
            var policy = JsonSerializer.Deserialize<InboxPolicy>("{\"read_receipts\":false,\"future_setting\":\"keep\"}", PostboxJson.Options)!;
            Require(policy.ReadReceipts == false && policy.Value("read_receipts") == "false" && policy.Other!["future_setting"].GetString() == "keep");
            return Task.CompletedTask;
        });
    }
    private const string Own = "/k/fixture_alice", Peer = "/k/fixture_bob", Ref = "mref_aaaaaaaaaaaaaaaaaaaaaaaaaa";
    private static ThreadMessage Build(InboxMessage message) => ConversationBuilder.Build(
        new([message], [], [], new HashSet<string>()), [], [new(Own)], new(Own), now: 200).Single().Messages.Single();
    private static void Require(bool value) { if (!value) throw new Exception("Message action assertion failed."); }
    private static void Reject(Action action) { try { action(); } catch (ArgumentException) { return; } throw new Exception("Invalid action accepted."); }
    private static async Task Invalid(Func<Task> action) { try { await action(); } catch (ArgumentException) { return; } throw new Exception("Invalid copy ID accepted."); }
    private static HttpResponseMessage Json(object value) => new(HttpStatusCode.OK)
    { Content = new StringContent(JsonSerializer.Serialize(value), Encoding.UTF8, "application/json") };
    private sealed class Tokens : IAccessTokenProvider
    {
        internal AuthorizationLease Root = new();
        public AuthorizationLease? AcceptedAuthorization => Root;
        internal int Acquisitions;
        public Task<string> GetTokenAsync(CancellationToken ct) { Acquisitions++; return Task.FromResult("synthetic-message-action-token"); }
        public Task<string> RefreshTokenAsync(CancellationToken ct) => GetTokenAsync(ct);
    }
    private sealed class Handler(Func<HttpRequestMessage, Task<HttpResponseMessage>> respond) : HttpMessageHandler
    {
        protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken ct) => respond(request);
    }
    private sealed class Fixture : IDisposable
    {
        internal readonly Tokens Tokens = new();
        private readonly HttpClient http;
        internal readonly PostboxClient Client;
        internal Fixture(Func<HttpRequestMessage, Task<HttpResponseMessage>> respond)
        { http = new(new Handler(respond)); Client = new(http, Tokens, new("http://127.0.0.1:9")); }
        public void Dispose() { Client.Dispose(); http.Dispose(); Tokens.Root.Retire(); }
    }
}
