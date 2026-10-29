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
            using var a = new PostboxClient(http, new Tokens(alice.GetProperty("token").GetString()!, 'a'), endpoint);
            using var b = new PostboxClient(http, new Tokens(bob.GetProperty("token").GetString()!, 'b'), endpoint);
            using var c = new PostboxClient(http, new Tokens(carol.GetProperty("token").GetString()!, 'c'), endpoint);
            using var loss = new PostboxClient(http, new Tokens(alice.GetProperty("token").GetString()!, 'a'), proxy);

            stage = "capabilities";
            var features = await a.GetCapabilitiesAsync(default);
            Check(new[] { ServerFeatures.MessageEdit, ServerFeatures.MessageReply, ServerFeatures.Receipts, ServerFeatures.AgentStatus, ServerFeatures.RecipientLookup }.All(features.Has));

            stage = "canonical recipient lookup";
            using (var lookup = await MessageRecoveryScope.AuthorizeAsync(a, from) ?? throw new InvalidOperationException())
            {
                var route = await a.ResolveRecipientAsync(lookup, to, default);
                Check(route.From == from && route.Address == to && (await a.GetInboxAsync(from)).Messages!.Count == 0 && (await b.GetInboxAsync(to)).Messages!.Count == 0);
            }

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

            stage = "protected restart recovery";
            var custodyFolder = Path.Combine(Path.GetTempPath(), "pp-windows-custody-" + Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(custodyFolder);
            try
            {
                using var protector = new MessageAttemptStoreTests.Protector();
                using var originalScope = await MessageRecoveryScope.AuthorizeAsync(loss, from) ?? throw new InvalidOperationException();
                var store = new FileMessageAttemptStore(custodyFolder, protector);
                var coordinator = new MessageDeliveryCoordinator(loss, store);
                var prepared = await coordinator.PrepareAsync(originalScope, new(from, to, "Protected restart fixture ğ 😀"), new(to, "Protected restart subject"));
                await Control(http, proxy, "after");
                await ExpectLoss(() => coordinator.SendAsync(originalScope, prepared, default));
                await CheckAttempts(http, proxy, 1, prepared.Request.MsgRef);
                originalScope.Dispose(); Check(!originalScope.IsCurrent);
                await Control(http, proxy, "off");
                var restartTokens = new Tokens(alice.GetProperty("token").GetString()!, 'a');
                using var restarted = new PostboxClient(http, restartTokens, proxy);
                using var currentScope = await MessageRecoveryScope.AuthorizeAsync(restarted, from) ?? throw new InvalidOperationException();
                var restartStore = new FileMessageAttemptStore(custodyFolder, protector);
                var restartCoordinator = new MessageDeliveryCoordinator(restarted, restartStore);
                var restored = (await restartCoordinator.GetPendingAsync(currentScope)).Single();
                Check(restored.Review == prepared.Review && restored.Request.To == to);
                var result = await restartCoordinator.SendAsync(currentScope, restored, default);
                Check(result.Receipt.Duplicate == true && result.Receipt.MsgRef == prepared.Request.MsgRef && !result.PendingCleanup);
                await CheckAttempts(http, proxy, 2, prepared.Request.MsgRef);
                Check((await b.GetInboxAsync(to)).Messages!.Count(m => m.MsgRef == prepared.Request.MsgRef) == 1);
                Check((await restartCoordinator.GetPendingAsync(currentScope)).Count == 0 && await restartStore.LoadAsync(currentScope, to, null) is null);
                restartTokens.AcceptedAuthorization?.Retire();
            }
            finally { Directory.Delete(custodyFolder, recursive: true); }
            stage = "new conversation opens after its first send";
            var carolAddress = carol.GetProperty("address").GetString()!;
            var openFolder = Path.Combine(Path.GetTempPath(), "pp-windows-open-" + Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(openFolder);
            try
            {
                using var protector = new MessageAttemptStoreTests.Protector();
                using var bobLoss = new PostboxClient(http, new Tokens(bob.GetProperty("token").GetString()!, 'b'), proxy);
                foreach (var (client, owner, lose) in new[] { (a, from, false), (bobLoss, to, true) })
                {
                    var coordinator = new MessageDeliveryCoordinator(client, new FileMessageAttemptStore(Path.Combine(openFolder, owner.Replace('/', '_')), protector));
                    using var model = new InboxViewModel(client, deliveryCoordinator: coordinator);
                    await model.InitializeAsync();
                    Check(model.SelectedMailbox?.Address == owner);
                    var text = lose ? "First message, answer lost" : "First message to a new recipient";
                    if (lose) await Control(http, proxy, "after");
                    await model.StartConversationAsync(carolAddress, text);
                    if (lose) await Control(http, proxy, "off");
                    Check(model.SelectedConversation?.Peer == carolAddress && model.SelectedMailbox?.Address == owner);
                    Check(model.Messages.Count(m => m.DisplayBody == text) == 1 && model.CanCompose);
                    if (lose) Check(model.HasPendingDeliveries);
                    await model.RefreshAsync(background: true);
                    Check(model.SelectedConversation?.Peer == carolAddress && model.Messages.Count(m => m.DisplayBody == text) == 1);
                    // The server's default thread now exists; an unconfirmed first message stays in the composer.
                    Check(model.Draft == (lose ? text : ""));
                }
            }
            finally { Directory.Delete(openFolder, recursive: true); }

            stage = "normal protected composer response loss";
            var composerFolder = Path.Combine(Path.GetTempPath(), "pp-windows-composer-" + Guid.NewGuid().ToString("N"));
            Directory.CreateDirectory(composerFolder);
            try
            {
                using var protector = new MessageAttemptStoreTests.Protector();
                var coordinator = new MessageDeliveryCoordinator(loss, new FileMessageAttemptStore(composerFolder, protector));
                using var model = new InboxViewModel(loss, deliveryCoordinator: coordinator);
                await model.InitializeAsync(); await model.StartConversationAsync(to, "");
                model.Draft = "Real postbox composer fixture ğ 😀";
                await Control(http, proxy, "after"); await model.SendDraftAsync();
                Check(model.Draft == "Real postbox composer fixture ğ 😀" && model.HasPendingDeliveries && !model.IsSending);
                using var scope = await MessageRecoveryScope.AuthorizeAsync(loss, from) ?? throw new InvalidOperationException();
                var saved = (await coordinator.GetPendingAsync(scope)).Single();
                await CheckAttempts(http, proxy, 1, saved.Request.MsgRef);
                Check((await b.GetInboxAsync(to)).Messages!.Count(m => m.MsgRef == saved.Request.MsgRef) == 1);
                await model.RefreshAsync(background: true);
                Check(model.Messages.Count(m => m.DisplayBody == "Real postbox composer fixture ğ 😀") == 1
                    && model.Draft == "Real postbox composer fixture ğ 😀" && model.HasPendingDeliveries);
                await Control(http, proxy, "off");
                Check(await model.RetryPendingDeliveryAsync(model.PendingDeliveries.Single().BeginAction() ?? throw new InvalidOperationException()));
                Check(model.Draft == "" && !model.HasPendingDeliveries && !model.IsSending);
                await CheckAttempts(http, proxy, 2, saved.Request.MsgRef);
                Check((await b.GetInboxAsync(to)).Messages!.Count(m => m.MsgRef == saved.Request.MsgRef) == 1);
                stage = "protected edit mode with committed response loss";
                model.SetMessageFeatures(features);
                model.Draft = "Restored ordinary interop draft";
                model.AddAttachments([new("mode_backup", "fixture.txt", 3, _ => Task.FromResult(new byte[] { 1, 2, 3 }))]);
                var original = model.Messages.Single(m => m.MsgRef == saved.Request.MsgRef);
                Check(model.BeginEdit(model.CaptureMessageTarget(original) ?? throw new InvalidOperationException()));
                model.Draft = "Edited via real postbox mode ğ 😀";
                await Control(http, proxy, "after"); await model.SendDraftAsync();
                Check(model.IsEditingMessage && model.Draft == "Edited via real postbox mode ğ 😀" && model.HasPendingDeliveries && !model.IsSending);
                var protectedEdit = (await coordinator.GetPendingAsync(scope)).Single();
                Check(protectedEdit.Request.EditOf == original.Id && protectedEdit.Request.ReplyTo is null && protectedEdit.Request.Attachments is null
                    && protectedEdit.Request.Body == RequestEnvelope.EditText(original.Body, "Edited via real postbox mode ğ 😀"));
                await CheckAttempts(http, proxy, 1, protectedEdit.Request.MsgRef);
                await model.RefreshAsync();
                Check(model.Messages.Count(m => m.MsgRef == original.MsgRef) == 1 && model.Messages.Single(m => m.MsgRef == original.MsgRef).EditCount == 1);
                await Control(http, proxy, "off");
                Check(await model.RetryPendingDeliveryAsync(model.PendingDeliveries.Single().BeginAction() ?? throw new InvalidOperationException()));
                Check(model.Draft == "Restored ordinary interop draft" && model.DraftAttachments.Count == 1 && !model.HasComposerAction && !model.HasPendingDeliveries);
                await CheckAttempts(http, proxy, 2, protectedEdit.Request.MsgRef);
                var edited = (await b.GetInboxAsync(to)).Messages!.Where(m => m.MsgRef == original.MsgRef).ToArray();
                Check(edited.Length == 1 && edited[0].EditCount == 1 && RequestEnvelope.DisplayText(edited[0].Body) == "Edited via real postbox mode ğ 😀");
                var ownTarget = model.CaptureMessageTarget(model.Messages.Single(m => m.MsgRef == original.MsgRef)) ?? throw new InvalidOperationException();
                var versions = await model.ReadMessageInfoAsync(ownTarget);
                Check(versions?.Versions?.Count == 2 && versions.Versions[0].Body == original.Body && versions.Versions[1].Body == protectedEdit.Request.Body);
                stage = "protected reply mode";
                model.RemoveAttachment(model.DraftAttachments.Single());
                var parent = model.Messages.First(m => !m.IsOutgoing && !m.IsSystem);
                Check(model.BeginReply(model.CaptureMessageTarget(parent) ?? throw new InvalidOperationException()));
                model.Draft = "Reply via real postbox mode"; await model.SendDraftAsync();
                var replyMode = (await b.GetInboxAsync(to)).Messages!.Single(m => RequestEnvelope.DisplayText(m.Body) == "Reply via real postbox mode");
                Check(replyMode.ReplyTo?.MsgRef == parent.MsgRef && replyMode.ReplyTo?.Quote == RequestEnvelope.ReplyQuote(parent.CopyText)
                    && model.Draft == "" && !model.HasComposerAction && !model.HasPendingDeliveries && !model.IsSending);
            }
            finally { Directory.Delete(composerFolder, recursive: true); }
            Console.WriteLine("PASS Windows postbox interoperability: canonical lookup, folded edits, replies, receipt privacy, owned copies, exact committed-loss, protected restart, new conversation opened after a confirmed or unconfirmed first send, normal/reply/edit composer recovery, owned version info and version deletion.");
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
    private sealed class Tokens(string value, char owner) : IAccessTokenProvider
    {
        public AuthorizationLease? AcceptedAuthorization { get; } = new();
        // Fixture-only stable subjects. Production derives this partition at the trusted exchange.
        public string? RecoveryOwnerKey => new string(owner, 64);
        public Task<string> GetTokenAsync(CancellationToken ct) => Task.FromResult(value);
        public Task<string> RefreshTokenAsync(CancellationToken ct) => Task.FromResult(value);
        public override string ToString() => "FixtureTokens(redacted)";
    }
}
