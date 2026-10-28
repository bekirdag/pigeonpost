using System.Security.Cryptography;
using System.Text.Json;
using System.Text.RegularExpressions;
using Pigeonpost.Core.Groups;

namespace Pigeonpost.Core.Meetings;

// Where this device keeps its notes key, one per signed-in person (the session's owner key, a hash
// of issuer, client and subject). A key is never read for anyone but the person it was saved for.
public interface INotesKeyStore
{
    Task<NotesDeviceKey?> LoadAsync(string owner);
    Task SaveAsync(string owner, NotesDeviceKey key);
    Task RemoveAsync(string owner);
}

public static partial class NotesOwner
{
    [GeneratedRegex("^[0-9a-f]{64}$")] private static partial Regex Pattern();
    public static bool Valid(string? owner) => owner is not null && Pattern().IsMatch(owner);
}

// The private key sealed with the Windows user's data protection (IStateProtector), written
// atomically, and checked on the way back in: same person, well-formed id, matching key pair.
public sealed class FileNotesKeyStore(string folder, IStateProtector protector) : INotesKeyStore
{
    private static readonly SemaphoreSlim Gate = new(1, 1);
    public static string DefaultFolder() => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Pigeonpost", "meeting-notes");
    private string PathOf(string owner) => NotesOwner.Valid(owner) ? Path.Combine(folder, owner + ".key") : throw new ArgumentException("Not an owner key.");

    private sealed record Stored(int V, string Owner, string KeyId, string PublicKey, string Pkcs8);

    public async Task<NotesDeviceKey?> LoadAsync(string owner)
    {
        var path = PathOf(owner);
        await Gate.WaitAsync().ConfigureAwait(false);
        try
        {
            if (!File.Exists(path)) return null;
            var sealedBytes = await File.ReadAllBytesAsync(path).ConfigureAwait(false);
            if (sealedBytes.Length is 0 or > 64 * 1024) throw new MeetingNotesException("bad_key");
            var plain = await protector.UnprotectAsync(sealedBytes).ConfigureAwait(false);
            try
            {
                var stored = JsonSerializer.Deserialize<Stored>(plain, PostboxJson.Options) ?? throw new MeetingNotesException("bad_key");
                if (stored.V != 1 || stored.Owner != owner) throw new MeetingNotesException("bad_key");
                var pkcs8 = Convert.FromBase64String(stored.Pkcs8);
                try { return NotesDeviceKey.Restore(stored.KeyId, stored.PublicKey, pkcs8); }
                finally { CryptographicOperations.ZeroMemory(pkcs8); }
            }
            catch (Exception ex) when (ex is JsonException or FormatException or CryptographicException) { throw new MeetingNotesException("bad_key"); }
            finally { CryptographicOperations.ZeroMemory(plain); }
        }
        finally { Gate.Release(); }
    }

    public async Task SaveAsync(string owner, NotesDeviceKey key)
    {
        var path = PathOf(owner);
        var plain = JsonSerializer.SerializeToUtf8Bytes(new Stored(1, owner, key.KeyId, key.PublicKey, Convert.ToBase64String(key.PrivatePkcs8)), PostboxJson.Options);
        await Gate.WaitAsync().ConfigureAwait(false);
        try
        {
            var sealedBytes = await protector.ProtectAsync(plain).ConfigureAwait(false);
            Directory.CreateDirectory(folder);
            var temporary = path + "." + Guid.NewGuid().ToString("N") + ".new";
            await File.WriteAllBytesAsync(temporary, sealedBytes).ConfigureAwait(false);
            File.Move(temporary, path, overwrite: true);
        }
        finally { CryptographicOperations.ZeroMemory(plain); Gate.Release(); }
    }

    public async Task RemoveAsync(string owner)
    {
        var path = PathOf(owner);
        await Gate.WaitAsync().ConfigureAwait(false);
        try { if (File.Exists(path)) File.Delete(path); }
        finally { Gate.Release(); }
    }
}

// Tests and the UI-test build: the same contract, in memory.
public sealed class MemoryNotesKeyStore : INotesKeyStore
{
    private readonly Dictionary<string, (string Id, string Public, byte[] Pkcs8)> keys = new(StringComparer.Ordinal);
    public int Saves { get; private set; }
    public Task<NotesDeviceKey?> LoadAsync(string owner)
    {
        lock (keys) return Task.FromResult(keys.TryGetValue(owner, out var k) ? NotesDeviceKey.Restore(k.Id, k.Public, k.Pkcs8) : null);
    }
    public Task SaveAsync(string owner, NotesDeviceKey key)
    {
        lock (keys) { keys[owner] = (key.KeyId, key.PublicKey, key.PrivatePkcs8.ToArray()); Saves++; }
        return Task.CompletedTask;
    }
    public Task RemoveAsync(string owner) { lock (keys) keys.Remove(owner); return Task.CompletedTask; }
    public IReadOnlyCollection<string> Owners { get { lock (keys) return keys.Keys.ToArray(); } }
}
