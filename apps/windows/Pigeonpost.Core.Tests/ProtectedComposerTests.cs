using Pigeonpost.Core;
using Pigeonpost.Core.Groups;

internal static class ProtectedComposerTests
{
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("The normal protected composer persists before sending and clears only its confirmed draft", async () =>
        {
            using var f = await Fixture.Create(); f.Model.Draft = "Normal protected fixture";
            var savedBeforePost = false; f.Service.BeforePost = () => savedBeforePost = Directory.EnumerateFiles(f.Folder, "*.pending", SearchOption.AllDirectories).Count() == 1;
            await f.Model.SendDraftAsync();
            Require(savedBeforePost && f.Service.Posts == 1 && f.Service.Committed.Count == 1 && f.Service.LegacyPosts == 0
                && f.Model.Draft == "" && !f.Model.HasPendingDeliveries && !f.Model.HasError && !f.Model.IsSending);
        });
        yield return ("A lost composer response preserves its draft and requires an explicit exact retry", async () =>
        {
            using var f = await Fixture.Create(); f.Model.Draft = "Lost composer fixture"; f.Service.LoseNext = true;
            await f.Model.SendDraftAsync();
            Require(f.Model.Draft == "Lost composer fixture" && f.Model.HasPendingDeliveries && f.Service.Posts == 1);
            await f.Model.RefreshAsync(background: true); await f.Model.RefreshPendingDeliveriesAsync();
            Require(f.Service.Posts == 1 && f.Model.Messages.Count(m => m.DisplayBody == "Lost composer fixture") == 1 && f.Model.Draft == "Lost composer fixture");
            var view = f.Model.PendingDeliveries.Single(); var target = view.BeginAction() ?? throw new Exception("Fixture target missing.");
            Require(await f.Model.RetryPendingDeliveryAsync(target));
            Require(f.Service.Posts == 2 && f.Service.Committed.Count == 1 && f.Model.Draft == "" && !f.Model.HasPendingDeliveries
                && view.Recipient == "" && view.BeginAction() is null && !f.Model.IsSending);
        });
        yield return ("Confirming a pending delivery cannot erase a later draft revision", async () =>
        {
            using var f = await Fixture.Create(); f.Model.Draft = "Original revision"; f.Service.LoseNext = true; await f.Model.SendDraftAsync();
            f.Model.Draft = "Later unsent revision";
            Require(await f.Model.RetryPendingDeliveryAsync(f.Model.PendingDeliveries.Single().BeginAction()!));
            Require(f.Model.Draft == "Later unsent revision" && f.Service.Committed.Count == 1 && !f.Model.HasPendingDeliveries);
        });
        yield return ("A composer protection failure sends nothing and preserves text and files", async () =>
        {
            using var f = await Fixture.Create(); f.Model.Draft = "Unsaved fixture"; f.Protection.Fail = true;
            f.Model.AddAttachments([new("draft_file", "fixture.txt", 3, _ => Task.FromResult(new byte[] { 1, 2, 3 }))]);
            await f.Model.SendDraftAsync();
            Require(f.Service.Posts == 0 && f.Model.Draft == "Unsaved fixture" && f.Model.DraftAttachments.Count == 1
                && f.Model.HasError && !f.Model.IsSending && !Directory.EnumerateFiles(f.Folder, "*.pending", SearchOption.AllDirectories).Any());
        });
        yield return ("An explicit attachment retry retains uploaded IDs without reading or uploading the files again", async () =>
        {
            using var f = await Fixture.Create(); f.Model.Draft = "Attached fixture"; f.Service.LoseNext = true; var reads = 0;
            f.Model.AddAttachments([new("draft_file", "fixture.txt", 3, _ => { reads++; return Task.FromResult(new byte[] { 1, 2, 3 }); })]);
            await f.Model.SendDraftAsync();
            Require(reads == 1 && f.Service.Uploads == 1 && f.Model.DraftAttachments.Count == 1 && f.Model.HasPendingDeliveries);
            Require(await f.Model.RetryPendingDeliveryAsync(f.Model.PendingDeliveries.Single().BeginAction()!));
            Require(reads == 1 && f.Service.Uploads == 1 && f.Service.Committed.Count == 1 && f.Model.DraftAttachments.Count == 0 && f.Model.Draft == "");
        });
        yield return ("Mailbox changes retire pending controls and keep each inbox's draft and recovery separate", async () =>
        {
            using var f = await Fixture.Create(); f.Model.Draft = "Original inbox fixture"; f.Service.LoseNext = true; await f.Model.SendDraftAsync();
            var originalMailbox = f.Model.SelectedMailbox!; var oldView = f.Model.PendingDeliveries.Single(); var oldTarget = oldView.BeginAction()!;
            await f.Model.SwitchMailboxAsync(f.Model.Mailboxes.Single(m => m.Address != originalMailbox.Address)); f.Model.Draft = "Other inbox fixture";
            Require(!f.Model.HasPendingDeliveries && oldView.Recipient == "" && oldView.BeginAction() is null && !await f.Model.RetryPendingDeliveryAsync(oldTarget));
            await f.Model.SwitchMailboxAsync(originalMailbox);
            Require(f.Model.Draft == "Original inbox fixture" && f.Model.HasPendingDeliveries && !oldTarget.IsCurrent && f.Service.Posts == 1);
        });
        yield return ("Discarding local recovery preserves the draft and makes no delivery or deletion call", async () =>
        {
            using var f = await Fixture.Create(); f.Model.Draft = "Discarded recovery fixture"; f.Service.LoseNext = true; await f.Model.SendDraftAsync();
            Require(await f.Model.DiscardPendingDeliveryAsync(f.Model.PendingDeliveries.Single().BeginAction()!));
            Require(f.Model.Draft == "Discarded recovery fixture" && !f.Model.HasPendingDeliveries && f.Service.Posts == 1 && f.Service.Committed.Count == 1);
        });
        yield return ("An unretired provider replacement during composer authorization cannot save or send under the new account", async () =>
        {
            using var f = await Fixture.Create(); f.Model.Draft = "Retired account fixture"; f.Service.BlockMailboxes = true;
            var send = f.Model.SendDraftAsync(); await f.Service.Waiting.Task.WaitAsync(TimeSpan.FromSeconds(3));
            var old = f.Service.Root; f.Service.Rotate(retire: false); await f.Model.InitializeAsync(); f.Model.Draft = "New account fixture";
            f.Service.Release.TrySetResult(); await send;
            Require(old.IsActive && f.Service.Posts == 0 && f.Model.Draft == "New account fixture" && !f.Model.HasPendingDeliveries && !f.Model.IsSending);
            old.Retire();
        });
        yield return ("A queued pending action retains its original caller through a fresh authorization delay", async () =>
        {
            using var f = await Fixture.Create(); f.Model.Draft = "Caller fixture"; f.Service.LoseNext = true; await f.Model.SendDraftAsync();
            var caller = f.Service.Root.Child(); PendingDeliveryTarget target;
            using (new AuthorizationContext(caller)) target = f.Model.PendingDeliveries.Single().BeginAction()!;
            f.Service.BlockMailboxes = true; var retry = f.Model.RetryPendingDeliveryAsync(target);
            await f.Service.Waiting.Task.WaitAsync(TimeSpan.FromSeconds(3)); caller.Retire(); f.Service.Release.TrySetResult();
            Require(!await retry && f.Service.Root.IsActive && f.Service.Posts == 1 && f.Model.Draft == "Caller fixture" && !f.Model.IsSending);
        });
        yield return ("An uncertain notification reply is protected and explicitly recoverable without consuming a composer draft", async () =>
        {
            using var f = await Fixture.Create(); f.Model.Draft = "Existing composer draft"; f.Service.LoseNext = true;
            Require(!await f.Model.ReplyFromAlertAsync(f.Model.SelectedMailbox!.Address, "/preview/design", "design-general", "Inline reply fixture"));
            Require(f.Model.HasPendingDeliveries && f.Model.Draft == "Existing composer draft" && f.Model.PendingDeliveries.Single().Body == "Inline reply fixture"
                && f.Service.Posts == 1 && f.Service.LegacyPosts == 0);
            Require(await f.Model.RetryPendingDeliveryAsync(f.Model.PendingDeliveries.Single().BeginAction()!));
            Require(f.Service.Committed.Count == 1 && f.Service.Posts == 2 && f.Model.Draft == "Existing composer draft" && !f.Model.HasPendingDeliveries);
        });
        yield return ("Notification reply protection failure prevents sending and preserves an existing composer draft", async () =>
        {
            using var f = await Fixture.Create(); f.Model.Draft = "Existing composer fixture"; f.Protection.Fail = true;
            Require(!await f.Model.ReplyFromAlertAsync(f.Model.SelectedMailbox!.Address, "/preview/design", "design-general", "Unsaved inline fixture"));
            Require(f.Service.Posts == 0 && f.Service.LegacyPosts == 0 && f.Model.Draft == "Existing composer fixture" && f.Model.HasError);
        });
    }
    private static void Require(bool value) { if (!value) throw new Exception("Protected composer assertion failed."); }
    private sealed class Fixture : IDisposable
    {
        internal readonly string Folder = Path.Combine(Path.GetTempPath(), "pp-composer-fixture-" + Guid.NewGuid().ToString("N"));
        internal readonly MessageAttemptStoreTests.Protector Cipher = new();
        internal readonly Protection Protection; internal readonly Service Service = new(); internal readonly InboxViewModel Model;
        private Fixture()
        {
            Directory.CreateDirectory(Folder); Protection = new(Cipher);
            Model = new(Service, deliveryCoordinator: new(Service, new FileMessageAttemptStore(Folder, Protection)));
        }
        internal static async Task<Fixture> Create()
        {
            var f = new Fixture(); await f.Model.InitializeAsync();
            f.Model.SelectConversation(f.Model.Conversations.Single(c => c.Peer == "/preview/design")); return f;
        }
        public void Dispose() { Service.Release.TrySetResult(); Model.Dispose(); Service.Root.Retire(); Cipher.Dispose(); Directory.Delete(Folder, recursive: true); }
    }
    private sealed class Protection(IStateProtector inner) : IStateProtector
    {
        internal bool Fail;
        public Task<byte[]> ProtectAsync(byte[] plain) => Fail ? throw new IOException("Synthetic protection failure.") : inner.ProtectAsync(plain);
        public Task<byte[]> UnprotectAsync(byte[] bytes) => inner.UnprotectAsync(bytes);
    }
    private sealed class Service : IProtectedMessageService, IAttachmentService
    {
        private PreviewInboxService preview = new();
        internal AuthorizationLease Root = new(); private char owner = 'a';
        public AuthorizationLease? AcceptedAuthorization => Root;
        public string? RecoveryOwnerKey => new string(owner, 64);
        public string? RecoveryOrigin => "http://127.0.0.1:9186";
        internal int Posts, LegacyPosts, Uploads;
        internal bool LoseNext, BlockMailboxes;
        internal Action? BeforePost;
        internal readonly Dictionary<string, SendReceipt> Committed = [];
        internal readonly TaskCompletionSource Waiting = new(TaskCreationOptions.RunContinuationsAsynchronously), Release = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal void Rotate(bool retire = true) { if (retire) Root.Retire(); Root = new(); owner = 'b'; preview = new(); }
        public async Task<IReadOnlyList<Mailbox>> GetMailboxesAsync(CancellationToken ct)
        {
            if (BlockMailboxes) { BlockMailboxes = false; Waiting.TrySetResult(); await Release.Task.ConfigureAwait(false); }
            return await preview.GetMailboxesAsync(ct);
        }
        public Task<RecipientRoute> ResolveRecipientAsync(MessageRecoveryScope scope, string peer, CancellationToken ct)
        { scope.RequireCurrent(ct); return Task.FromResult(new RecipientRoute(scope.Mailbox, "/k/bbbbbbbbbbbbbbbbbbbbbbbbbb", peer)); }
        public async Task<SendReceipt> SendPreparedMessageAsync(MessageRecoveryScope scope, PreparedMessageAttempt attempt, CancellationToken ct)
        {
            scope.RequireCurrent(ct); BeforePost?.Invoke(); Posts++;
            if (Committed.TryGetValue(attempt.Request.MsgRef, out var receipt)) return receipt with { Duplicate = true };
            var sent = await preview.SendAsync(scope.Mailbox, attempt.Review.Recipient, attempt.Request.Body, attempt.Request.ThreadId, ct);
            receipt = sent with { MsgRef = attempt.Request.MsgRef, Duplicate = false }; Committed[attempt.Request.MsgRef] = receipt;
            if (LoseNext) { LoseNext = false; throw new HttpRequestException("Synthetic response loss."); }
            return receipt;
        }
        public Task<MessageAttachment> UploadAsync(string identity, string filename, byte[] bytes, CancellationToken ct)
        { ct.ThrowIfCancellationRequested(); Uploads++; return Task.FromResult(new MessageAttachment("uploaded_fixture_" + Uploads, filename, "application/octet-stream", bytes.Length)); }
        public Task<SendReceipt> SendAsync(string identity, string peer, string body, string? threadId, CancellationToken ct)
        { LegacyPosts++; throw new InvalidOperationException("Protected fixture cannot use legacy send."); }
        public Task<SendReceipt> SendAttachmentsAsync(string identity, string peer, string body, string? threadId, IReadOnlyList<string> ids, CancellationToken ct)
        { LegacyPosts++; throw new InvalidOperationException("Protected fixture cannot use legacy attachment send."); }
        public async Task<InboxSnapshot> LoadAsync(string identity, CancellationToken ct)
        {
            var snapshot = await preview.LoadAsync(identity, ct);
            var references = Committed.Values.Where(r => r.SentCopyId is not null).ToDictionary(r => r.SentCopyId!, r => r.MsgRef);
            return snapshot with { Messages = snapshot.Messages.Select(m => m.IsOutgoing && references.TryGetValue(m.MessageId, out var reference) ? m with { MsgRef = reference } : m).ToArray() };
        }
        public Task<string> CreateThreadAsync(string identity, string peer, string title, CancellationToken ct) => preview.CreateThreadAsync(identity, peer, title, ct);
        public Task SetArchivedAsync(string identity, string peer, bool archived, CancellationToken ct) => preview.SetArchivedAsync(identity, peer, archived, ct);
        public Task AcknowledgeAsync(string identity, string messageId, CancellationToken ct) => preview.AcknowledgeAsync(identity, messageId, ct);
        public Task SetContactAsync(string identity, Contact contact, CancellationToken ct) => preview.SetContactAsync(identity, contact, ct);
        public Task RemoveContactAsync(string identity, string peer, CancellationToken ct) => preview.RemoveContactAsync(identity, peer, ct);
        public Task DeleteThreadAsync(string identity, string threadId, CancellationToken ct) => preview.DeleteThreadAsync(identity, threadId, ct);
    }
}
