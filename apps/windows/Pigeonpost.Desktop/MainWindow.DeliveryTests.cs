#if UI_TESTS
using System.Text;
using Pigeonpost.Core;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow
{
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

    private sealed class NativeDeliveryFixture : IProtectedMessageService
    {
        internal const string Own = "/k/aaaaaaaaaaaaaaaaaaaaaaaaaa", Peer = "/k/bbbbbbbbbbbbbbbbbbbbbbbbbb";
        internal AuthorizationLease Root = new(); private char owner = 'a';
        public AuthorizationLease? AcceptedAuthorization => Root;
        public string? RecoveryOwnerKey => new string(owner, 64);
        public string? RecoveryOrigin => "https://native-delivery-fixture.invalid";
        internal int Posts; internal bool LoseNext;
        internal readonly HashSet<string> Committed = [];
        internal void Rotate(char key) { Root.Retire(); Root = new(); owner = key; }
        public Task<IReadOnlyList<Mailbox>> GetMailboxesAsync(CancellationToken ct) => Task.FromResult<IReadOnlyList<Mailbox>>([new(Own)]);
        public Task<RecipientRoute> ResolveRecipientAsync(MessageRecoveryScope scope, string peer, CancellationToken ct)
        { scope.RequireCurrent(ct); return Task.FromResult(new RecipientRoute(Own, Peer, peer)); }
        public Task<SendReceipt> SendPreparedMessageAsync(MessageRecoveryScope scope, PreparedMessageAttempt attempt, CancellationToken ct)
        {
            scope.RequireCurrent(ct); Posts++; var duplicate = !Committed.Add(attempt.Request.MsgRef);
            if (LoseNext) { LoseNext = false; throw new HttpRequestException("Synthetic committed response loss."); }
            return Task.FromResult(new SendReceipt("native_recipient_copy", "native_sent_copy", attempt.Request.MsgRef, duplicate));
        }
        public Task<InboxSnapshot> LoadAsync(string identity, CancellationToken ct) => throw new NotSupportedException();
        public Task<SendReceipt> SendAsync(string identity, string peer, string body, string? threadId, CancellationToken ct) => throw new NotSupportedException();
        public Task<string> CreateThreadAsync(string identity, string peer, string title, CancellationToken ct) => throw new NotSupportedException();
        public Task SetArchivedAsync(string identity, string peer, bool archived, CancellationToken ct) => throw new NotSupportedException();
        public Task AcknowledgeAsync(string identity, string messageId, CancellationToken ct) => throw new NotSupportedException();
        public Task SetContactAsync(string identity, Contact contact, CancellationToken ct) => throw new NotSupportedException();
        public Task RemoveContactAsync(string identity, string peer, CancellationToken ct) => throw new NotSupportedException();
        public Task DeleteThreadAsync(string identity, string threadId, CancellationToken ct) => throw new NotSupportedException();
    }
}
#endif
