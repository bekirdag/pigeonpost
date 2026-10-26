using System.Net;
using System.Net.Http.Json;
using System.Text.Json;
using Pigeonpost.Core;

// Opt-in protocol test: only the disposable fixture in tests/e2e-messages/postbox.mjs.
internal static class MessageActionsPostbox
{
    private static string stage = "setup";
    internal static async Task<int> RunAsync()
    {
        try
        {
            var file = Environment.GetEnvironmentVariable("PIGEONPOST_TEST_FIXTURE") ?? throw new InvalidOperationException();
            using var setup = JsonDocument.Parse(await File.ReadAllTextAsync(file));
            var root = setup.RootElement;
            var endpoint = Local(root.GetProperty("postbox").GetString()!);
            var proxy = Local(root.GetProperty("lossProxy").GetString()!);
            var people = root.GetProperty("people");
            var alice = people.GetProperty("alice"); var bob = people.GetProperty("bob"); var carol = people.GetProperty("carol");
            var from = alice.GetProperty("address").GetString()!; var to = bob.GetProperty("address").GetString()!;
            using var handler = new HttpClientHandler { AllowAutoRedirect = false };
            using var http = new HttpClient(handler) { Timeout = TimeSpan.FromSeconds(15) };
            using var a = new PostboxClient(http, new Tokens(alice.GetProperty("token").GetString()!), endpoint);
            using var b = new PostboxClient(http, new Tokens(bob.GetProperty("token").GetString()!), endpoint);
            using var c = new PostboxClient(http, new Tokens(carol.GetProperty("token").GetString()!), endpoint);
            using var loss = new PostboxClient(http, new Tokens(alice.GetProperty("token").GetString()!), proxy);

            stage = "capabilities";
            var features = await a.GetCapabilitiesAsync(default);
            Check(new[] { ServerFeatures.MessageEdit, ServerFeatures.MessageReply, ServerFeatures.Receipts, ServerFeatures.AgentStatus }.All(features.Has));

            stage = "send and distinct copies";
            var sent = await a.SendMessageAsync(new(from, to, "Original fixture message"), default);
            Check(sent.MessageId is not null && sent.SentCopyId is not null && sent.MessageId != sent.SentCopyId && MessageReferences.IsValid(sent.MsgRef!));
            var incoming = (await b.GetInboxAsync(to)).Messages!.Single(m => m.MsgRef == sent.MsgRef);
            Check(incoming.MessageId == sent.MessageId && incoming.Receipt is null && incoming.AgentStatus is null);
            var sender = (await a.GetInboxAsync(from)).Messages!.Single(m => m.MsgRef == sent.MsgRef);
            Check(sender.Receipt?.State == "delivered");

            stage = "folded edit and version acknowledgement";
            await a.SendMessageAsync(new(from, to, "Revised fixture message", editOf: sent.MsgRef), default);
            var folded = (await b.GetInboxAsync(to)).Messages!.Where(m => m.MsgRef == sent.MsgRef).ToArray();
            Check(folded.Length == 1 && folded[0].Body == "Revised fixture message" && folded[0].EditCount == 1 && folded[0].Read == false);
            var info = await b.GetMessageInfoAsync(to, sent.MessageId!, default);
            Check(info.Versions?.Count == 2 && info.Versions[0].Body == "Original fixture message" && info.Versions[1].Body == "Revised fixture message");
            await b.AcknowledgeAsync(to, sent.MessageId!, default);
            Check((await b.GetInboxAsync(to)).Messages!.Single(m => m.MsgRef == sent.MsgRef).Read == true);
            Check((await a.GetInboxAsync(from)).Messages!.Single(m => m.MsgRef == sent.MsgRef).Receipt?.State == "seen");

            stage = "reply and own-copy authorization";
            var reply = await b.SendMessageAsync(new(to, from, "Reply fixture message", replyTo: sent.MessageId, quote: "Remembered fixture quote"), default);
            var replyRow = (await a.GetInboxAsync(from)).Messages!.Single(m => m.MsgRef == reply.MsgRef);
            Check(replyRow.ReplyTo is { } context && context.MsgRef == sent.MsgRef && context.Quote == "Remembered fixture quote");
            try { await c.GetMessageInfoAsync(carol.GetProperty("address").GetString()!, sent.MessageId!, default); throw new InvalidOperationException(); }
            catch (PostboxException ex) when (ex.StatusCode is 403 or 404) { }

            stage = "read receipt privacy";
            var policy = await b.SetPolicyAsync(to, "read_receipts", false, default); Check(policy.ReadReceipts == false);
            var privateSent = await a.SendMessageAsync(new(from, to, "Receipt privacy fixture"), default);
            await b.GetInboxAsync(to); await b.AcknowledgeAsync(to, privateSent.MessageId!, default);
            Check((await a.GetInboxAsync(from)).Messages!.Single(m => m.MsgRef == privateSent.MsgRef).Receipt?.State == "delivered");

            stage = "committed send response loss";
            var request = new MessageSendRequest(from, to, "Committed loss fixture");
            await Control(http, proxy, "after");
            await ExpectLoss(() => loss.SendMessageAsync(request, default));
            await CheckAttempts(http, proxy, 1, request.MsgRef);
            Check((await b.GetInboxAsync(to)).Messages!.Count(m => m.MsgRef == request.MsgRef) == 1);
            await Control(http, proxy, "off");
            var recovered = await loss.SendMessageAsync(request, default);
            Check(recovered.Duplicate == true && recovered.MsgRef == request.MsgRef);
            await CheckAttempts(http, proxy, 2, request.MsgRef);

            stage = "committed edit response loss";
            var edit = new MessageSendRequest(from, to, "Recovered revised fixture", editOf: request.MsgRef);
            await Control(http, proxy, "after");
            await ExpectLoss(() => loss.SendMessageAsync(edit, default));
            await CheckAttempts(http, proxy, 1, edit.MsgRef);
            await Control(http, proxy, "off");
            Check((await loss.SendMessageAsync(edit, default)).Duplicate == true);
            await CheckAttempts(http, proxy, 2, edit.MsgRef);
            Check((await b.GetMessageInfoAsync(to, recovered.MessageId!, default)).Versions?.Count == 2);

            stage = "whole-version deletion";
            await b.DeleteMessageAsync(to, recovered.MessageId!, default);
            Check((await b.GetInboxAsync(to)).Messages!.All(m => m.MsgRef != request.MsgRef));
            Check((await a.GetMessageInfoAsync(from, recovered.SentCopyId!, default)).Versions?.Count == 2);
            Console.WriteLine("PASS Windows postbox interoperability: folded edits, replies, receipt privacy, owned copies, exact committed-loss recovery and version deletion.");
            return 0;
        }
        catch (Exception ex)
        {
            // Do not print parser input, fixture credentials, HTTP headers, URLs or response bodies.
            Console.Error.WriteLine($"FAIL Windows message-action interoperability at {stage} ({ex.GetType().Name}).");
            return 1;
        }
    }
    private static Uri Local(string value)
    {
        var uri = new Uri(value);
        if (!uri.IsLoopback || uri.Scheme != "http" || !string.IsNullOrEmpty(uri.UserInfo)
            || uri.AbsolutePath != "/" || uri.Query.Length != 0 || uri.Fragment.Length != 0) throw new InvalidOperationException();
        return uri;
    }
    private static void Check(bool value) { if (!value) throw new InvalidOperationException(); }
    private static async Task Control(HttpClient http, Uri proxy, string mode)
    {
        using var response = await http.PostAsJsonAsync(new Uri(proxy, "/__test/control"), new { mode });
        response.EnsureSuccessStatusCode();
    }
    private static async Task ExpectLoss(Func<Task> action)
    {
        try { await action(); } catch (HttpRequestException) { return; } catch (IOException) { return; }
        throw new InvalidOperationException();
    }
    private static async Task CheckAttempts(HttpClient http, Uri proxy, int expected, string reference)
    {
        using var response = await http.GetAsync(new Uri(proxy, "/__test/attempts")); response.EnsureSuccessStatusCode();
        using var body = JsonDocument.Parse(await response.Content.ReadAsStringAsync());
        var attempts = body.RootElement.GetProperty("attempts").EnumerateArray().ToArray();
        Check(attempts.Length == expected && attempts.All(a => a.GetProperty("msgRef").GetString() == reference));
        Check(attempts.All(a => a.GetProperty("sha").GetString() == attempts[0].GetProperty("sha").GetString()));
        Check(attempts.All(a => a.GetProperty("status").GetInt32() is 200 or 201));
    }
    private sealed class Tokens(string value) : IAccessTokenProvider
    {
        public AuthorizationLease? AcceptedAuthorization { get; } = new();
        public Task<string> GetTokenAsync(CancellationToken ct) => Task.FromResult(value);
        public Task<string> RefreshTokenAsync(CancellationToken ct) => Task.FromResult(value);
        public override string ToString() => "FixtureTokens(redacted)";
    }
}
