using System.Security.Cryptography;
using System.Text;
using Pigeonpost.Core;
using Pigeonpost.Core.Groups;

internal static class MessageAttemptStoreTests
{
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Protected pending requests survive a client-store restart without plaintext files or a new message reference", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            var attempt = PreparedMessageAttempt.Create(scope, new(Own, Peer, "Private fixture contents", threadId: "subject_fixture", replyTo: "parent_copy", quote: "Stored quote"));
            await f.Store.SaveAsync(scope, attempt);
            var blob = File.ReadAllBytes(f.Files().Single());
            Require(!Encoding.UTF8.GetString(blob).Contains("Private fixture contents", StringComparison.Ordinal));
            var restarted = new FileMessageAttemptStore(f.Folder, f.Protector);
            var restored = await restarted.LoadAsync(scope, Peer, "subject_fixture") ?? throw new Exception("Pending fixture missing.");
            Require(restored.Request.MsgRef == attempt.Request.MsgRef && restored.Request.Body == "Private fixture contents"
                && restored.Request.ReplyTo == "parent_copy" && restored.Request.Quote == "Stored quote" && restored.CreatedAt == attempt.CreatedAt);
            Require(restored.ToString() == "PreparedMessageAttempt(redacted)");
        });
        yield return ("Unresolved delivery intents cannot overwrite each other or be removed by a different reference", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            var first = PreparedMessageAttempt.Create(scope, new(Own, Peer, "First intent"));
            await f.Store.SaveAsync(scope, first); var original = File.ReadAllBytes(f.Files().Single());
            await f.Store.SaveAsync(scope, first);
            var different = PreparedMessageAttempt.Create(scope, new(Own, Peer, "Replacement intent"));
            await Fails<MessageAttemptConflictException>(() => f.Store.SaveAsync(scope, different));
            await Fails<MessageAttemptConflictException>(() => f.Store.RemoveAsync(scope, Peer, null, different.Request.MsgRef));
            Require(original.SequenceEqual(File.ReadAllBytes(f.Files().Single())) && (await f.Store.LoadAsync(scope, Peer, null))?.Request.MsgRef == first.Request.MsgRef);
            Require(await f.Store.RemoveAsync(scope, Peer, null, first.Request.MsgRef) && !await f.Store.RemoveAsync(scope, Peer, null, first.Request.MsgRef));
        });
        yield return ("Corrupt protected state blocks replacement and remains available for diagnosis", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            await f.Store.SaveAsync(scope, PreparedMessageAttempt.Create(scope, new(Own, Peer, "First intent")));
            var path = f.Files().Single(); var broken = File.ReadAllBytes(path); broken[^1] ^= 1; File.WriteAllBytes(path, broken);
            await Fails<MessageAttemptUnavailableException>(() => f.Store.LoadAsync(scope, Peer, null));
            await Fails<MessageAttemptUnavailableException>(() => f.Store.SaveAsync(scope, PreparedMessageAttempt.Create(scope, new(Own, Peer, "Replacement intent"))));
            Require(File.ReadAllBytes(path).SequenceEqual(broken));
        });
        yield return ("Changing accounts or postbox origins cannot expose another partition's pending request", async () =>
        {
            using var f = new Fixture(); using var first = await f.Scope();
            var attempt = PreparedMessageAttempt.Create(first, new(Own, Peer, "Original owner fixture")); await f.Store.SaveAsync(first, attempt);
            f.Service.Rotate(); f.Service.Key = new string('b', 64);
            using var other = await f.Scope();
            Require(await f.Store.LoadAsync(other, Peer, null) is null);
            await Fails<AuthorizationRetiredException>(() => f.Store.SaveAsync(other, attempt));
            f.Service.Rotate(); f.Service.Key = new string('a', 64); f.Service.Origin = "https://different-postbox.invalid";
            using var differentOrigin = await f.Scope();
            Require(await f.Store.LoadAsync(differentOrigin, Peer, null) is null && f.Files().Length == 1);
        });
        yield return ("Fresh same-owner authorization can recover after reauthentication without reviving the old scope", async () =>
        {
            using var f = new Fixture(); using var old = await f.Scope();
            var attempt = PreparedMessageAttempt.Create(old, new(Own, Peer, "Restart fixture")); await f.Store.SaveAsync(old, attempt);
            f.Service.Rotate(); using var current = await f.Scope();
            Require(!old.IsCurrent && (await f.Store.LoadAsync(current, Peer, null))?.Request.MsgRef == attempt.Request.MsgRef);
            await Fails<AuthorizationRetiredException>(() => f.Store.LoadAsync(old, Peer, null));
        });
        yield return ("A valid encrypted file copied across owner partitions cannot authorize its embedded contents", async () =>
        {
            using var f = new Fixture(); using var first = await f.Scope();
            await f.Store.SaveAsync(first, PreparedMessageAttempt.Create(first, new(Own, Peer, "Original owner fixture")));
            var original = f.Files().Single(); f.Service.Rotate(); f.Service.Key = new string('b', 64);
            using var second = await f.Scope(); await f.Store.SaveAsync(second, PreparedMessageAttempt.Create(second, new(Own, Peer, "Second owner fixture")));
            var secondPath = f.Files().Single(p => p != original); File.Copy(original, secondPath, overwrite: true);
            await Fails<MessageAttemptUnavailableException>(() => f.Store.LoadAsync(second, Peer, null));
            Require(File.Exists(secondPath));
        });
        yield return ("Retiring a caller during encryption prevents publication and clears its plaintext buffer", async () =>
        {
            using var f = new Fixture(); var caller = f.Service.Root.Child();
            using var context = new AuthorizationContext(caller); using var scope = await f.Scope();
            var blocker = new BlockingProtector(f.Protector); var store = new FileMessageAttemptStore(f.Folder, blocker);
            var save = store.SaveAsync(scope, PreparedMessageAttempt.Create(scope, new(Own, Peer, "Interrupted fixture")));
            await blocker.Started.Task.WaitAsync(TimeSpan.FromSeconds(3)); caller.Retire(); blocker.Release.TrySetResult();
            await Fails<AuthorizationRetiredException>(() => save);
            Require(f.Files().Length == 0 && blocker.Plain is not null && blocker.Plain.All(b => b == 0));
            Require(!Directory.EnumerateFiles(f.Folder, "*.tmp", SearchOption.AllDirectories).Any());
        });
        yield return ("Two store instances cannot prepare conflicting requests while another process lock is held", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope(); var blocker = new BlockingProtector(f.Protector);
            var slow = new FileMessageAttemptStore(f.Folder, blocker); var fast = new FileMessageAttemptStore(f.Folder, f.Protector);
            var first = PreparedMessageAttempt.Create(scope, new(Own, Peer, "First concurrent fixture"));
            var save = slow.SaveAsync(scope, first); await blocker.Started.Task.WaitAsync(TimeSpan.FromSeconds(3));
            try { await Fails<MessageAttemptUnavailableException>(() => fast.SaveAsync(scope, PreparedMessageAttempt.Create(scope, new(Own, Peer, "Other concurrent fixture")))); }
            finally { blocker.Release.TrySetResult(); }
            await save; Require((await fast.LoadAsync(scope, Peer, null))?.Request.MsgRef == first.Request.MsgRef);
        });
        yield return ("Durable requests require frozen canonical sender and recipient addresses", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            await Fails<ArgumentException>(() => { PreparedMessageAttempt.Create(scope, new(Own, "/recipient/main", "Alias fixture")); return Task.CompletedTask; });
            await Fails<ArgumentException>(() => { PreparedMessageAttempt.Create(scope, new(Peer, Peer, "Wrong sender fixture")); return Task.CompletedTask; });
            Require(f.Files().Length == 0);
        });
        yield return ("An authorized restarted inbox discovers the original reviewed handle without resolving it again", async () =>
        {
            using var f = new Fixture(); using var original = await f.Scope();
            var context = new MessageDeliveryReview("/original-handle/main", "Interview plans", new string('c', 32));
            var attempt = PreparedMessageAttempt.Create(original, new(Own, Peer, "Restarted review fixture", threadId: "original_subject"), context);
            await f.Store.SaveAsync(original, attempt); f.Service.Rotate(); using var current = await f.Scope();
            var restarted = new FileMessageAttemptStore(f.Folder, f.Protector);
            var restored = (await restarted.ListAsync(current)).Single();
            Require(restored.Request.To == Peer && restored.Request.MsgRef == attempt.Request.MsgRef && restored.Review == context);
            await Fails<AuthorizationRetiredException>(() => restarted.ListAsync(original));
            f.Service.Rotate(); f.Service.Key = new string('b', 64); using var other = await f.Scope();
            Require((await restarted.ListAsync(other)).Count == 0 && f.Files().Length == 1);
        });
        yield return ("A pending request cannot silently replace its reviewed delivery context", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            var request = new MessageSendRequest(Own, Peer, "Unchanged wire fixture");
            await f.Store.SaveAsync(scope, PreparedMessageAttempt.Create(scope, request, new("/original/main", "Original subject")));
            await Fails<MessageAttemptConflictException>(() => f.Store.SaveAsync(scope, PreparedMessageAttempt.Create(scope, request, new("/different/main", "Other subject"))));
            Require((await f.Store.ListAsync(scope)).Single().Review.Recipient == "/original/main");
        });
        yield return ("A renamed protected request cannot become another mailbox slot", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            await f.Store.SaveAsync(scope, PreparedMessageAttempt.Create(scope, new(Own, Peer, "Renamed fixture")));
            var original = f.Files().Single(); var copied = Path.Combine(Path.GetDirectoryName(original)!, new string('0', 32) + ".pending");
            File.Copy(original, copied);
            await Fails<MessageAttemptUnavailableException>(() => f.Store.ListAsync(scope));
            Require(f.Files().Length == 2);
        });
        yield return ("Pending discovery refuses a corrupted entry without erasing it or publishing a partial list", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            await f.Store.SaveAsync(scope, PreparedMessageAttempt.Create(scope, new(Own, Peer, "Valid fixture")));
            var invalid = Path.Combine(Path.GetDirectoryName(f.Files().Single())!, "invalid.pending"); File.WriteAllBytes(invalid, [1, 2, 3]);
            await Fails<MessageAttemptUnavailableException>(() => f.Store.ListAsync(scope));
            Require(f.Files().Length == 2 && File.ReadAllBytes(invalid).SequenceEqual(new byte[] { 1, 2, 3 }));
        });
        yield return ("Retiring a caller during pending discovery prevents publication and clears decrypted bytes", async () =>
        {
            using var f = new Fixture(); var caller = f.Service.Root.Child();
            using var context = new AuthorizationContext(caller); using var scope = await f.Scope();
            await f.Store.SaveAsync(scope, PreparedMessageAttempt.Create(scope, new(Own, Peer, "Interrupted read fixture")));
            var blocker = new BlockingReader(f.Protector); var restarted = new FileMessageAttemptStore(f.Folder, blocker);
            var list = restarted.ListAsync(scope); await blocker.Started.Task.WaitAsync(TimeSpan.FromSeconds(3));
            caller.Retire(); blocker.Release.TrySetResult(); await Fails<AuthorizationRetiredException>(() => list);
            Require(blocker.Plain is not null && blocker.Plain.All(b => b == 0) && f.Files().Length == 1 && f.Service.Root.IsActive);
        });
        yield return ("Pending delivery capacity is bounded per inbox and an explicitly removed slot can be reused", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope(); var references = new List<string>();
            for (var i = 0; i < 64; i++)
            {
                var attempt = PreparedMessageAttempt.Create(scope, new(Own, Peer, "Bounded fixture", threadId: "subject_" + i));
                await f.Store.SaveAsync(scope, attempt); references.Add(attempt.Request.MsgRef);
            }
            var extra = PreparedMessageAttempt.Create(scope, new(Own, Peer, "Extra fixture", threadId: "extra_subject"));
            await Fails<MessageAttemptLimitException>(() => f.Store.SaveAsync(scope, extra));
            Require((await f.Store.ListAsync(scope)).Count == 64);
            Require(await f.Store.RemoveAsync(scope, Peer, "subject_0", references[0])); await f.Store.SaveAsync(scope, extra);
            Require((await f.Store.ListAsync(scope)).Count == 64 && (await f.Store.ListAsync(scope)).All(a => a.Request.MsgRef != references[0]));
        });
        yield return ("A transferred handle cannot create a second pending intent in a different canonical slot", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope(); var review = new MessageDeliveryReview("/transferred/main");
            var original = PreparedMessageAttempt.Create(scope, new(Own, Peer, "Original recipient fixture"), review);
            await f.Store.SaveAsync(scope, original);
            var replacement = PreparedMessageAttempt.Create(scope, new(Own, "/k/cccccccccccccccccccccccccc", "Different recipient fixture"), new("/transferred"));
            await Fails<MessageAttemptConflictException>(() => new FileMessageAttemptStore(f.Folder, f.Protector).SaveAsync(scope, replacement));
            Require((await f.Store.ListAsync(scope)).Single().Request.MsgRef == original.Request.MsgRef);
        });
        yield return ("Original v1 encrypted requests remain discoverable with canonical display context", async () =>
        {
            using var f = new Fixture(); using var scope = await f.Scope();
            var original = PreparedMessageAttempt.Create(scope, new(Own, Peer, "Legacy envelope fixture")); await f.Store.SaveAsync(scope, original);
            var path = f.Files().Single(); var sealedBytes = File.ReadAllBytes(path); var plain = await f.Protector.UnprotectAsync(sealedBytes[5..]);
            var envelope = System.Text.Json.Nodes.JsonNode.Parse(plain)!;
            envelope["version"] = 1; envelope.AsObject().Remove("review"); CryptographicOperations.ZeroMemory(plain);
            var legacy = Encoding.UTF8.GetBytes(envelope.ToJsonString());
            try { File.WriteAllBytes(path, "PPMA1"u8.ToArray().Concat(await f.Protector.ProtectAsync(legacy)).ToArray()); }
            finally { CryptographicOperations.ZeroMemory(legacy); }
            var restored = (await f.Store.ListAsync(scope)).Single();
            Require(restored.Request.MsgRef == original.Request.MsgRef && restored.Review == new MessageDeliveryReview(Peer));
        });
    }
    private const string Own = "/k/aaaaaaaaaaaaaaaaaaaaaaaaaa", Peer = "/k/bbbbbbbbbbbbbbbbbbbbbbbbbb";
    private static void Require(bool value) { if (!value) throw new Exception("Pending delivery store assertion failed."); }
    private static async Task Fails<T>(Func<Task> action) where T : Exception
    { try { await action(); } catch (T) { return; } throw new Exception("Pending delivery unexpectedly accepted."); }
    private sealed class Fixture : IDisposable
    {
        internal readonly string Folder = Path.Combine(Path.GetTempPath(), "pp-attempt-tests-" + Guid.NewGuid().ToString("N"));
        internal readonly Service Service = new();
        internal readonly Protector Protector = new();
        internal readonly FileMessageAttemptStore Store;
        internal Fixture() { Directory.CreateDirectory(Folder); Store = new(Folder, Protector); }
        internal async Task<MessageRecoveryScope> Scope() => await MessageRecoveryScope.AuthorizeAsync(Service, Own) ?? throw new Exception("Fixture scope missing.");
        internal string[] Files() => Directory.GetFiles(Folder, "*.pending", SearchOption.AllDirectories);
        public void Dispose() { Service.Root.Retire(); Protector.Dispose(); Directory.Delete(Folder, recursive: true); }
    }
    internal sealed class Protector : IStateProtector, IDisposable
    {
        private readonly byte[] key = RandomNumberGenerator.GetBytes(32);
        public Task<byte[]> ProtectAsync(byte[] plain)
        {
            var nonce = RandomNumberGenerator.GetBytes(12); var cipher = new byte[plain.Length]; var tag = new byte[16];
            using var aes = new AesGcm(key, 16); aes.Encrypt(nonce, plain, cipher, tag);
            return Task.FromResult(nonce.Concat(tag).Concat(cipher).ToArray());
        }
        public Task<byte[]> UnprotectAsync(byte[] data)
        {
            if (data.Length < 28) throw new CryptographicException(); var plain = new byte[data.Length - 28];
            using var aes = new AesGcm(key, 16); aes.Decrypt(data.AsSpan(0, 12), data.AsSpan(28), data.AsSpan(12, 16), plain); return Task.FromResult(plain);
        }
        public void Dispose() => CryptographicOperations.ZeroMemory(key);
    }
    private sealed class BlockingProtector(IStateProtector inner) : IStateProtector
    {
        internal readonly TaskCompletionSource Started = new(TaskCreationOptions.RunContinuationsAsynchronously), Release = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal byte[]? Plain;
        public async Task<byte[]> ProtectAsync(byte[] plain)
        {
            Plain = plain; var encrypted = await inner.ProtectAsync(plain); Started.TrySetResult(); await Release.Task.ConfigureAwait(false); return encrypted;
        }
        public Task<byte[]> UnprotectAsync(byte[] data) => inner.UnprotectAsync(data);
    }
    private sealed class BlockingReader(IStateProtector inner) : IStateProtector
    {
        internal readonly TaskCompletionSource Started = new(TaskCreationOptions.RunContinuationsAsynchronously), Release = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal byte[]? Plain;
        public Task<byte[]> ProtectAsync(byte[] plain) => inner.ProtectAsync(plain);
        public async Task<byte[]> UnprotectAsync(byte[] data)
        { Plain = await inner.UnprotectAsync(data); Started.TrySetResult(); await Release.Task.ConfigureAwait(false); return Plain; }
    }
    private sealed class Service : IInboxService
    {
        internal AuthorizationLease Root = new();
        internal string Key = new string('a', 64), Origin = "http://127.0.0.1:9185";
        public AuthorizationLease? AcceptedAuthorization => Root;
        public string? RecoveryOwnerKey => Key;
        public string? RecoveryOrigin => Origin;
        internal void Rotate() { Root.Retire(); Root = new(); }
        public Task<IReadOnlyList<Mailbox>> GetMailboxesAsync(CancellationToken ct) => Task.FromResult<IReadOnlyList<Mailbox>>([new(Own)]);
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
