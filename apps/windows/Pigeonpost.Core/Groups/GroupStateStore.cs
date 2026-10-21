using System.Security.Cryptography;
using System.Text;
using System.Text.Json;

namespace Pigeonpost.Core.Groups;

// Encrypts the device record at rest. Windows uses DPAPI for the signed-in user (Desktop); the
// tests use a fixed-key protector. Unprotect must fail (throw) for bytes it did not protect.
public interface IStateProtector
{
    Task<byte[]> ProtectAsync(byte[] plain);
    Task<byte[]> UnprotectAsync(byte[] sealedBytes);
}

public interface IGroupStateStore
{
    bool Persistent { get; }
    Task<GroupDeviceRecord?> LoadAsync(string mailbox);
    Task SaveAsync(string mailbox, GroupDeviceRecord record);
    Task RemoveAsync(string mailbox);
}

public sealed class MemoryGroupStateStore : IGroupStateStore
{
    private readonly Dictionary<string, string> rows = new(StringComparer.Ordinal);
    public bool Persistent => false;
    public Task<GroupDeviceRecord?> LoadAsync(string mailbox) =>
        Task.FromResult(rows.TryGetValue(mailbox, out var json) ? JsonSerializer.Deserialize<GroupDeviceRecord>(json) : null);
    public Task SaveAsync(string mailbox, GroupDeviceRecord record) { rows[mailbox] = JsonSerializer.Serialize(record); return Task.CompletedTask; }
    public Task RemoveAsync(string mailbox) { rows.Remove(mailbox); return Task.CompletedTask; }
}

// One file per mailbox under the app's local data folder. The record (MLS state blob, its state
// key and the decrypted history) is serialized, sealed by the protector and written atomically:
// a crash leaves either the previous file or the new one, never a mix of an old blob and newer
// cursors. A file the protector cannot open (another Windows user, a reset profile) is treated as
// no state: this device then enrols again as a new device.
public sealed class FileGroupStateStore(string folder, IStateProtector protector) : IGroupStateStore
{
    private static readonly byte[] Magic = "PPGS1"u8.ToArray();
    private readonly SemaphoreSlim gate = new(1, 1);
    public bool Persistent => true;

    public static string DefaultFolder() => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Pigeonpost", "groups");

    private string PathFor(string mailbox) => Path.Combine(folder,
        Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(mailbox)))[..32].ToLowerInvariant() + ".state");

    public async Task<GroupDeviceRecord?> LoadAsync(string mailbox)
    {
        await gate.WaitAsync().ConfigureAwait(false);
        try
        {
            var path = PathFor(mailbox);
            if (!File.Exists(path)) return null;
            var bytes = await File.ReadAllBytesAsync(path).ConfigureAwait(false);
            if (bytes.Length < Magic.Length || !bytes.AsSpan(0, Magic.Length).SequenceEqual(Magic)) return null;
            byte[] plain;
            try { plain = await protector.UnprotectAsync(bytes[Magic.Length..]).ConfigureAwait(false); }
            catch (Exception) { return null; }
            try { return JsonSerializer.Deserialize<GroupDeviceRecord>(plain); }
            catch (JsonException) { return null; }
            finally { CryptographicOperations.ZeroMemory(plain); }
        }
        finally { gate.Release(); }
    }

    public async Task SaveAsync(string mailbox, GroupDeviceRecord record)
    {
        var plain = JsonSerializer.SerializeToUtf8Bytes(record);
        byte[] sealedBytes;
        try { sealedBytes = await protector.ProtectAsync(plain).ConfigureAwait(false); }
        finally { CryptographicOperations.ZeroMemory(plain); }
        await gate.WaitAsync().ConfigureAwait(false);
        try
        {
            Directory.CreateDirectory(folder);
            var path = PathFor(mailbox);
            var temporary = path + ".tmp";
            await using (var file = new FileStream(temporary, FileMode.Create, FileAccess.Write, FileShare.None, 4096, FileOptions.WriteThrough))
            {
                await file.WriteAsync(Magic).ConfigureAwait(false);
                await file.WriteAsync(sealedBytes).ConfigureAwait(false);
            }
            File.Move(temporary, path, overwrite: true);
        }
        finally { gate.Release(); }
    }

    public async Task RemoveAsync(string mailbox)
    {
        await gate.WaitAsync().ConfigureAwait(false);
        try { File.Delete(PathFor(mailbox)); }
        finally { gate.Release(); }
    }
}

// AES-GCM under a fixed key: for tests and the UI-test build only. Never used by the Store app.
public sealed class FixedKeyProtector(byte[] key) : IStateProtector
{
    public Task<byte[]> ProtectAsync(byte[] plain)
    {
        var nonce = RandomNumberGenerator.GetBytes(12);
        var output = new byte[12 + 16 + plain.Length];
        using var aes = new AesGcm(key, 16);
        aes.Encrypt(nonce, plain, output.AsSpan(28), output.AsSpan(12, 16));
        nonce.CopyTo(output, 0);
        return Task.FromResult(output);
    }

    public Task<byte[]> UnprotectAsync(byte[] sealedBytes)
    {
        if (sealedBytes.Length < 28) throw new CryptographicException("short");
        var plain = new byte[sealedBytes.Length - 28];
        using var aes = new AesGcm(key, 16);
        aes.Decrypt(sealedBytes.AsSpan(0, 12), sealedBytes.AsSpan(28), sealedBytes.AsSpan(12, 16), plain);
        return Task.FromResult(plain);
    }
}
