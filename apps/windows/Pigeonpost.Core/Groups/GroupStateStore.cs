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

// The live group actor must use these overloads; a local lease grants no server membership.
public interface IAuthorizedGroupStateStore : IGroupStateStore
{
    Task<GroupDeviceRecord?> LoadAsync(string mailbox, AuthorizationLease authorization);
    Task SaveAsync(string mailbox, GroupDeviceRecord record, AuthorizationLease authorization);
    Task RemoveAsync(string mailbox, AuthorizationLease authorization);
}

public sealed class GroupStateUnavailableException() : IOException("Saved encrypted group state could not be opened. Try again before resetting this device.");

public sealed class MemoryGroupStateStore : IAuthorizedGroupStateStore
{
    private readonly Dictionary<string, string> rows = new(StringComparer.Ordinal);
    private readonly object monitor = new();
    public bool Persistent => false;
    private T Local<T>(AuthorizationLease? authorization, Func<T> work)
    {
        T Locked() { lock (monitor) return work(); }
        return authorization is null ? Locked() : authorization.WithActive(Locked);
    }
    public Task<GroupDeviceRecord?> LoadAsync(string mailbox) => Load(mailbox, null);
    public Task<GroupDeviceRecord?> LoadAsync(string mailbox, AuthorizationLease authorization) => Load(mailbox, authorization);
    private Task<GroupDeviceRecord?> Load(string mailbox, AuthorizationLease? authorization) => Task.FromResult(Local(authorization,
        () => rows.TryGetValue(mailbox, out var json) ? JsonSerializer.Deserialize<GroupDeviceRecord>(json) : null));
    public Task SaveAsync(string mailbox, GroupDeviceRecord record) => Save(mailbox, record, null);
    public Task SaveAsync(string mailbox, GroupDeviceRecord record, AuthorizationLease authorization) => Save(mailbox, record, authorization);
    private Task Save(string mailbox, GroupDeviceRecord record, AuthorizationLease? authorization)
    {
        Local(authorization, () => { rows[mailbox] = JsonSerializer.Serialize(record); return true; }); return Task.CompletedTask;
    }
    public Task RemoveAsync(string mailbox) => Remove(mailbox, null);
    public Task RemoveAsync(string mailbox, AuthorizationLease authorization) => Remove(mailbox, authorization);
    private Task Remove(string mailbox, AuthorizationLease? authorization)
    {
        Local(authorization, () => rows.Remove(mailbox)); return Task.CompletedTask;
    }
}

// One file per mailbox under the app's local data folder. The record (MLS state blob, its state
// key and the decrypted history) is serialized, sealed by the protector and written atomically:
// a crash leaves either the previous file or the new one, never a mix of an old blob and newer
// cursors. A file the protector cannot open (another Windows user, a reset profile) is treated as
// no state: this device then enrols again as a new device.
public sealed class FileGroupStateStore(string folder, IStateProtector protector) : IAuthorizedGroupStateStore
{
    private static readonly byte[] Magic = "PPGS1"u8.ToArray();
    private readonly SemaphoreSlim gate = new(1, 1);
    public bool Persistent => true;

    public static string DefaultFolder() => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Pigeonpost", "groups");

    private string PathFor(string mailbox) => Path.Combine(folder,
        Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(mailbox)))[..32].ToLowerInvariant() + ".state");

    private static T Local<T>(AuthorizationLease? authorization, Func<T> work) => authorization is null ? work() : authorization.WithActive(work);
    public Task<GroupDeviceRecord?> LoadAsync(string mailbox) => Load(mailbox, null);
    public Task<GroupDeviceRecord?> LoadAsync(string mailbox, AuthorizationLease authorization) => Load(mailbox, authorization);
    private async Task<GroupDeviceRecord?> Load(string mailbox, AuthorizationLease? authorization)
    {
        authorization?.RequireActive();
        await gate.WaitAsync().ConfigureAwait(false);
        try
        {
            authorization?.RequireActive();
            var path = PathFor(mailbox);
            if (!File.Exists(path)) return null;
            var bytes = await File.ReadAllBytesAsync(path).ConfigureAwait(false);
            authorization?.RequireActive();
            if (bytes.Length < Magic.Length || !bytes.AsSpan(0, Magic.Length).SequenceEqual(Magic))
            {
                if (authorization is not null) throw new GroupStateUnavailableException();
                return null;
            }
            byte[] plain;
            try { plain = await protector.UnprotectAsync(bytes[Magic.Length..]).ConfigureAwait(false); }
            catch (OperationCanceledException) { throw; }
            catch (Exception)
            {
                authorization?.RequireActive();
                if (authorization is not null) throw new GroupStateUnavailableException();
                return null;
            }
            try
            {
                return Local(authorization, () => JsonSerializer.Deserialize<GroupDeviceRecord>(plain)
                    ?? (authorization is not null ? throw new GroupStateUnavailableException() : null));
            }
            catch (JsonException)
            {
                authorization?.RequireActive();
                if (authorization is not null) throw new GroupStateUnavailableException();
                return null;
            }
            finally { CryptographicOperations.ZeroMemory(plain); }
        }
        finally { gate.Release(); }
    }

    public Task SaveAsync(string mailbox, GroupDeviceRecord record) => Save(mailbox, record, null);
    public Task SaveAsync(string mailbox, GroupDeviceRecord record, AuthorizationLease authorization) => Save(mailbox, record, authorization);
    private async Task Save(string mailbox, GroupDeviceRecord record, AuthorizationLease? authorization)
    {
        var plain = Local(authorization, () => JsonSerializer.SerializeToUtf8Bytes(record));
        byte[] sealedBytes;
        try { sealedBytes = await protector.ProtectAsync(plain).ConfigureAwait(false); }
        finally { CryptographicOperations.ZeroMemory(plain); }
        authorization?.RequireActive();
        await gate.WaitAsync().ConfigureAwait(false);
        string? temporary = null;
        try
        {
            Local(authorization, () => Directory.CreateDirectory(folder));
            var path = PathFor(mailbox);
            temporary = path + "." + Guid.NewGuid().ToString("N") + ".tmp";
            await using (var file = new FileStream(temporary, FileMode.CreateNew, FileAccess.Write, FileShare.None, 4096, FileOptions.WriteThrough))
            {
                await file.WriteAsync(Magic).ConfigureAwait(false);
                authorization?.RequireActive();
                await file.WriteAsync(sealedBytes).ConfigureAwait(false);
            }
            Local(authorization, () => { File.Move(temporary, path, overwrite: true); return true; });
        }
        finally
        {
            if (temporary is not null) try { File.Delete(temporary); } catch (IOException) { } catch (UnauthorizedAccessException) { }
            gate.Release();
        }
    }

    public Task RemoveAsync(string mailbox) => Remove(mailbox, null);
    public Task RemoveAsync(string mailbox, AuthorizationLease authorization) => Remove(mailbox, authorization);
    private async Task Remove(string mailbox, AuthorizationLease? authorization)
    {
        authorization?.RequireActive();
        await gate.WaitAsync().ConfigureAwait(false);
        try { Local(authorization, () => { File.Delete(PathFor(mailbox)); return true; }); }
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
