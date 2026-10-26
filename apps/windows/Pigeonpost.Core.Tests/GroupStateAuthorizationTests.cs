using System.Security.Cryptography;
using Pigeonpost.Core;
using Pigeonpost.Core.Groups;

internal static class GroupStateAuthorizationTests
{
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("An old memory-store actor cannot replace or erase a new actor's record", async () =>
        {
            var store = new MemoryGroupStateStore(); var old = new AuthorizationLease(); var current = new AuthorizationLease();
            var record = Row("original"); await store.SaveAsync(Mailbox, record, old); record.DeviceId = "changed after save";
            Require((await store.LoadAsync(Mailbox, old))!.DeviceId == "original");
            old.Retire(); await store.SaveAsync(Mailbox, Row("replacement"), current);
            await Fails<AuthorizationRetiredException>(() => store.SaveAsync(Mailbox, Row("late"), old));
            await Fails<AuthorizationRetiredException>(() => store.RemoveAsync(Mailbox, old));
            await Fails<AuthorizationRetiredException>(() => store.LoadAsync(Mailbox, old));
            Require((await store.LoadAsync(Mailbox, current))!.DeviceId == "replacement");
        });
        yield return ("Late group protection cannot overwrite the replacement actor's real file", async () =>
        {
            using var folder = new Folder(); var protector = new FixedKeyProtector(RandomNumberGenerator.GetBytes(32));
            var store = new FileGroupStateStore(folder.Path, protector); var old = new AuthorizationLease();
            await store.SaveAsync(Mailbox, Row("initial"), old);
            var blocked = new PausedProtector(protector, protecting: true);
            var pending = new FileGroupStateStore(folder.Path, blocked).SaveAsync(Mailbox, Row("late private record"), old);
            await blocked.Started.Task.WaitAsync(TimeSpan.FromSeconds(5));
            var current = new AuthorizationLease(); old.Retire();
            try { await store.SaveAsync(Mailbox, Row("replacement"), current); }
            finally { blocked.Release.TrySetResult(); }
            await Fails<AuthorizationRetiredException>(() => pending);
            Require((await store.LoadAsync(Mailbox, current))!.DeviceId == "replacement");
            Require(!Directory.EnumerateFiles(folder.Path, "*.tmp").Any());
        });
        yield return ("Late group unprotection cannot return a retired actor's plaintext", async () =>
        {
            using var folder = new Folder(); var protector = new FixedKeyProtector(RandomNumberGenerator.GetBytes(32));
            var store = new FileGroupStateStore(folder.Path, protector); var old = new AuthorizationLease();
            await store.SaveAsync(Mailbox, Row("saved"), old);
            var blocked = new PausedProtector(protector, protecting: false);
            var pending = new FileGroupStateStore(folder.Path, blocked).LoadAsync(Mailbox, old);
            await blocked.Started.Task.WaitAsync(TimeSpan.FromSeconds(5));
            old.Retire(); blocked.Release.SetResult();
            await Fails<AuthorizationRetiredException>(() => pending);
            Require((await store.LoadAsync(Mailbox, new AuthorizationLease()))!.DeviceId == "saved");
        });
        yield return ("A retired file-store actor cannot erase a replacement state", async () =>
        {
            using var folder = new Folder(); var store = new FileGroupStateStore(folder.Path, new FixedKeyProtector(RandomNumberGenerator.GetBytes(32)));
            var old = new AuthorizationLease(); var current = new AuthorizationLease(); old.Retire();
            await store.SaveAsync(Mailbox, Row("replacement"), current);
            await Fails<AuthorizationRetiredException>(() => store.RemoveAsync(Mailbox, old));
            Require((await store.LoadAsync(Mailbox, current))!.DeviceId == "replacement");
        });
        yield return ("Unreadable guarded group state is a refusal rather than an empty device", async () =>
        {
            using var folder = new Folder(); var store = new FileGroupStateStore(folder.Path, new FixedKeyProtector(RandomNumberGenerator.GetBytes(32)));
            var actor = new AuthorizationLease(); const string privateMarker = "Synthetic private device marker";
            Require(await store.LoadAsync(Mailbox, actor) is null);
            await store.SaveAsync(Mailbox, Row(privateMarker), actor);
            var path = Directory.EnumerateFiles(folder.Path, "*.state").Single(); var bytes = await File.ReadAllBytesAsync(path);
            bytes[^1] ^= 0xff; await File.WriteAllBytesAsync(path, bytes);
            var error = await Fails<GroupStateUnavailableException>(() => store.LoadAsync(Mailbox, actor));
            Require(error.InnerException is null && !error.Message.Contains(privateMarker));
            Require(await store.LoadAsync(Mailbox) is null); // Preserve the pre-existing unguarded API contract until the engine migrates.
        });
        yield return ("Independent group stores stage concurrent writes without sharing a temporary file", async () =>
        {
            using var folder = new Folder(); var protector = new FixedKeyProtector(RandomNumberGenerator.GetBytes(32)); var actor = new AuthorizationLease();
            var records = Enumerable.Range(0, 4).Select(i => new GroupDeviceRecord { DeviceId = "candidate-" + i, StateBlob = new string((char)('A' + i), 256 * 1024) }).ToArray();
            await Task.WhenAll(records.Select(record => new FileGroupStateStore(folder.Path, protector).SaveAsync(Mailbox, record, actor)));
            var actual = await new FileGroupStateStore(folder.Path, protector).LoadAsync(Mailbox, actor);
            var expected = records.Single(x => x.DeviceId == actual!.DeviceId);
            Require(actual!.StateBlob == expected.StateBlob && !Directory.EnumerateFiles(folder.Path, "*.tmp").Any());
        });
    }
    private const string Mailbox = "/k/groupfixture";
    private static GroupDeviceRecord Row(string name) => new() { DeviceId = name };
    private static TaskCompletionSource Signal() => new(TaskCreationOptions.RunContinuationsAsynchronously);
    private static void Require(bool condition) { if (!condition) throw new Exception("Group state authorization assertion failed."); }
    private static async Task<T> Fails<T>(Func<Task> action) where T : Exception
    {
        try { await action().WaitAsync(TimeSpan.FromSeconds(5)); }
        catch (T error) { return error; }
        throw new Exception("Expected " + typeof(T).Name);
    }
    private sealed class PausedProtector(IStateProtector inner, bool protecting) : IStateProtector
    {
        public readonly TaskCompletionSource Started = Signal(), Release = Signal();
        public async Task<byte[]> ProtectAsync(byte[] plain)
        {
            if (protecting) { Started.TrySetResult(); await Release.Task; }
            return await inner.ProtectAsync(plain);
        }
        public async Task<byte[]> UnprotectAsync(byte[] bytes)
        {
            if (!protecting) { Started.TrySetResult(); await Release.Task; }
            return await inner.UnprotectAsync(bytes);
        }
    }
    private sealed class Folder : IDisposable
    {
        public string Path { get; } = Directory.CreateTempSubdirectory("pp-group-actor-").FullName;
        public void Dispose() => Directory.Delete(Path, recursive: true);
    }
}
