#if UI_TESTS
using System.Text;
using Pigeonpost.Core;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow
{
    private NativeDeliveryFixture? deliveryUiFixture;
    // Runs under the real Windows standard user alongside native notification qualification.
    // The service is a synthetic in-process fixture without any network or account credentials.
    private static async Task RunDeliveryProtectionTestsAsync(Action<bool, string> check)
    {
        var folder = Path.Combine(Path.GetTempPath(), "Pigeonpost-delivery-fixture-" + Guid.NewGuid().ToString("N"));
        Directory.CreateDirectory(folder);
        var service = new NativeDeliveryFixture();
        try
        {
            var store = new FileMessageAttemptStore(folder, new DpapiProtector());
            var coordinator = new MessageDeliveryCoordinator(service, store);
            using var original = await MessageRecoveryScope.AuthorizeAsync(service, NativeDeliveryFixture.Own) ?? throw new InvalidOperationException("Native fixture authorization missing.");
            const string body = "Native Windows protected delivery fixture ğ 😀";
            var prepared = await coordinator.PrepareAsync(original, new(NativeDeliveryFixture.Own, "/fixture/original", body), new("/fixture/original", "Native fixture subject"));
            var blob = File.ReadAllBytes(Directory.GetFiles(folder, "*.pending", SearchOption.AllDirectories).Single());
            check(!Encoding.UTF8.GetString(blob).Contains(body, StringComparison.Ordinal), "Windows user protection encrypts the pending delivery file");
            service.LoseNext = true;
            var lost = false;
            try { await coordinator.SendAsync(original, prepared); }
            catch (HttpRequestException) { lost = true; }
            check(lost && service.Posts == 1 && service.Committed.Count == 1, "A lost confirmed response retains protected recovery without an automatic second send");
            original.Dispose(); service.Rotate('b');
            using (var other = await MessageRecoveryScope.AuthorizeAsync(service, NativeDeliveryFixture.Own) ?? throw new InvalidOperationException())
            {
                check((await coordinator.GetPendingAsync(other)).Count == 0, "A different signed-in owner cannot discover another owner's Windows-protected delivery");
            }
            service.Rotate('a');
            using var current = await MessageRecoveryScope.AuthorizeAsync(service, NativeDeliveryFixture.Own) ?? throw new InvalidOperationException();
            var restarted = new MessageDeliveryCoordinator(service, new FileMessageAttemptStore(folder, new DpapiProtector()));
            var recovered = (await restarted.GetPendingAsync(current)).Single();
            check(recovered.Request.MsgRef == prepared.Request.MsgRef && recovered.Request.To == NativeDeliveryFixture.Peer
                && recovered.Request.Body == body && recovered.Review == prepared.Review && service.Posts == 1,
                "A fresh Windows protector and store recover the exact original recipient, content, reference and review context");
            var result = await restarted.SendAsync(current, recovered);
            check(result.Receipt.Duplicate == true && !result.PendingCleanup && service.Posts == 2 && service.Committed.Count == 1,
                "Explicit native recovery retries the saved request and confirms one recipient copy");
            check((await restarted.GetPendingAsync(current)).Count == 0 && !Directory.EnumerateFiles(folder, "*.pending", SearchOption.AllDirectories).Any(),
                "Confirmed native recovery clears only the original protected pending entry");
        }
        finally { service.Root.Retire(); Directory.Delete(folder, recursive: true); }
    }

    private sealed class NativeDeliveryFixture(PreviewInboxService? preview = null, string? diagnosticFile = null) : IProtectedMessageService
    {
        internal const string Own = "/k/aaaaaaaaaaaaaaaaaaaaaaaaaa", Peer = "/k/bbbbbbbbbbbbbbbbbbbbbbbbbb";
        internal AuthorizationLease Root = new(); private char owner = 'a';
        public AuthorizationLease? AcceptedAuthorization => Root;
        public string? RecoveryOwnerKey => new string(owner, 64);
        public string? RecoveryOrigin => "https://native-delivery-fixture.invalid";
        internal int Posts; internal bool LoseNext;
        internal readonly HashSet<string> Committed = [];
        private readonly Dictionary<string, SendReceipt> receipts = [];
        private readonly Dictionary<string, string> intents = [];
        private bool exactRetries = true;
        private void Diagnostics()
        {
            if (diagnosticFile is null) return;
            var staging = diagnosticFile + ".tmp";
            File.WriteAllText(staging, System.Text.Json.JsonSerializer.Serialize(new { posts = Posts, committed = Committed.Count, exactRetries }));
            File.Move(staging, diagnosticFile, overwrite: true);
        }
        internal void Rotate(char key) { Root.Retire(); Root = new(); owner = key; }
        public Task<IReadOnlyList<Mailbox>> GetMailboxesAsync(CancellationToken ct) => preview?.GetMailboxesAsync(ct) ?? Task.FromResult<IReadOnlyList<Mailbox>>([new(Own)]);
        public Task<RecipientRoute> ResolveRecipientAsync(MessageRecoveryScope scope, string peer, CancellationToken ct)
        { scope.RequireCurrent(ct); return Task.FromResult(new RecipientRoute(scope.Mailbox, Peer, peer)); }
        public async Task<SendReceipt> SendPreparedMessageAsync(MessageRecoveryScope scope, PreparedMessageAttempt attempt, CancellationToken ct)
        {
            scope.RequireCurrent(ct); Posts++;
            var reference = attempt.Request.MsgRef;
            var intent = System.Text.Json.JsonSerializer.Serialize(attempt.Request);
            if (receipts.TryGetValue(reference, out var existing))
            {
                exactRetries &= intents[reference] == intent; Diagnostics(); return existing with { Duplicate = true };
            }
            var sent = preview is null ? new SendReceipt("native_recipient_copy", "native_sent_copy")
                : await preview.SendAsync(scope.Mailbox, attempt.Review.Recipient, attempt.Request.Body, attempt.Request.ThreadId, ct);
            scope.RequireCurrent(ct);
            var receipt = sent with { MsgRef = reference, Duplicate = false };
            Committed.Add(reference); receipts[reference] = receipt; intents[reference] = intent; Diagnostics();
            if (LoseNext || preview is not null) { LoseNext = false; throw new HttpRequestException("Synthetic committed response loss."); }
            return receipt;
        }
        public async Task<InboxSnapshot> LoadAsync(string identity, CancellationToken ct)
        {
            if (preview is null) throw new NotSupportedException();
            var snapshot = await preview.LoadAsync(identity, ct);
            var references = receipts.Values.Where(r => r.SentCopyId is not null).ToDictionary(r => r.SentCopyId!, r => r.MsgRef);
            return snapshot with { Messages = snapshot.Messages.Select(m => m.IsOutgoing && references.TryGetValue(m.MessageId, out var reference) ? m with { MsgRef = reference } : m).ToArray() };
        }
        public Task<SendReceipt> SendAsync(string identity, string peer, string body, string? threadId, CancellationToken ct) => throw new NotSupportedException();
        public Task<string> CreateThreadAsync(string identity, string peer, string title, CancellationToken ct) => preview?.CreateThreadAsync(identity, peer, title, ct) ?? throw new NotSupportedException();
        public Task SetArchivedAsync(string identity, string peer, bool archived, CancellationToken ct) => preview?.SetArchivedAsync(identity, peer, archived, ct) ?? throw new NotSupportedException();
        public Task AcknowledgeAsync(string identity, string messageId, CancellationToken ct) => preview?.AcknowledgeAsync(identity, messageId, ct) ?? throw new NotSupportedException();
        public Task SetContactAsync(string identity, Contact contact, CancellationToken ct) => preview?.SetContactAsync(identity, contact, ct) ?? throw new NotSupportedException();
        public Task RemoveContactAsync(string identity, string peer, CancellationToken ct) => preview?.RemoveContactAsync(identity, peer, ct) ?? throw new NotSupportedException();
        public Task DeleteThreadAsync(string identity, string threadId, CancellationToken ct) => throw new NotSupportedException();
    }
}
#endif
