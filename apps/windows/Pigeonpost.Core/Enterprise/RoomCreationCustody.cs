using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Pigeonpost.Core.Groups;

namespace Pigeonpost.Core.Enterprise;

public sealed class RoomCreationUnavailableException() : IOException("The saved room creation could not be opened. Nothing new was sent.");
public sealed class RoomCreationConflictException() : InvalidOperationException("Review the earlier room creation before starting another.");

// One uncertain organization-room creation per server-authorized canonical inbox, kept with its exact
// bytes until the person confirms the result or discards it. A login owner hint alone never opens it.
public sealed class RoomCreationAttempt
{
    public OrganizationRoomCreate Payload { get; }
    public long CreatedAt { get; }
    // Known once a creation receipt arrived but the final read did not: check it, never create again.
    public string? CandidateId { get; }
    internal string OwnerKey { get; }
    internal string Origin { get; }
    public string Mailbox { get; }
    internal RoomCreationAttempt(MessageRecoveryScope scope, OrganizationRoomCreate payload, long createdAt, string? candidateId = null)
    {
        scope.RequireCurrent();
        if (createdAt is < 0 or > 253402300799 || candidateId is not null && !OrganizationRoomInput.Identifier(candidateId)) throw new ArgumentException("This room creation is invalid.");
        Payload = payload; CreatedAt = createdAt; CandidateId = candidateId; OwnerKey = scope.OwnerKey; Origin = scope.Origin; Mailbox = scope.Mailbox;
    }
    internal RoomCreationAttempt WithCandidate(MessageRecoveryScope scope, string id) => new(scope, Payload, CreatedAt, id);
    // The exact request may be repeated only within a day; afterwards only a check or a discard remains.
    public bool CanRetry(long now) { try { var age = checked(now - CreatedAt); return CandidateId is null && age is >= 0 and <= 86400; } catch (OverflowException) { return false; } }
    internal void RequireScope(MessageRecoveryScope scope)
    {
        scope.RequireCurrent();
        if (OwnerKey != scope.OwnerKey || Origin != scope.Origin || Mailbox != scope.Mailbox) throw new AuthorizationRetiredException();
    }
    internal bool SameRequest(RoomCreationAttempt other) => Payload.Encoded == other.Payload.Encoded && CreatedAt == other.CreatedAt
        && OwnerKey == other.OwnerKey && Origin == other.Origin && Mailbox == other.Mailbox;
    internal static string Hash(string text) => Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(text)));
    public override string ToString() => "RoomCreationAttempt(redacted)";
}

public interface IRoomCreationStore
{
    bool Persistent { get; }
    Task<RoomCreationAttempt?> LoadAsync(MessageRecoveryScope scope, CancellationToken ct = default);
    // Saves a new attempt, or records the candidate of the same exact request. Anything else conflicts.
    Task SaveAsync(MessageRecoveryScope scope, RoomCreationAttempt attempt, CancellationToken ct = default);
    Task<bool> RemoveAsync(MessageRecoveryScope scope, RoomCreationAttempt expected, CancellationToken ct = default);
}

// Tests and fixtures only: in-process custody with the same scope and conflict rules.
public sealed class MemoryRoomCreationStore : IRoomCreationStore
{
    private readonly Dictionary<(string, string, string), RoomCreationAttempt> entries = [];
    public bool Persistent => false;
    public Task<RoomCreationAttempt?> LoadAsync(MessageRecoveryScope scope, CancellationToken ct = default)
    { scope.RequireCurrent(ct); return Task.FromResult(entries.GetValueOrDefault((scope.OwnerKey, scope.Origin, scope.Mailbox))); }
    public Task SaveAsync(MessageRecoveryScope scope, RoomCreationAttempt attempt, CancellationToken ct = default)
    {
        attempt.RequireScope(scope); ct.ThrowIfCancellationRequested();
        var key = (scope.OwnerKey, scope.Origin, scope.Mailbox);
        if (entries.TryGetValue(key, out var existing) && !existing.SameRequest(attempt)) throw new RoomCreationConflictException();
        scope.Local(() => entries[key] = attempt); return Task.CompletedTask;
    }
    public Task<bool> RemoveAsync(MessageRecoveryScope scope, RoomCreationAttempt expected, CancellationToken ct = default)
    {
        expected.RequireScope(scope); ct.ThrowIfCancellationRequested();
        var key = (scope.OwnerKey, scope.Origin, scope.Mailbox);
        if (!entries.TryGetValue(key, out var existing)) return Task.FromResult(false);
        if (!existing.SameRequest(expected)) throw new RoomCreationConflictException();
        return Task.FromResult(scope.Local(() => entries.Remove(key)));
    }
}

// Operating-system protected file custody (DPAPI on Windows), separate from message and calendar files.
public sealed class FileRoomCreationStore(string folder, IStateProtector protector) : IRoomCreationStore
{
    private static readonly byte[] Magic = "PPRC1"u8.ToArray();
    private const int MaxPlainBytes = 64 * 1024, MaxSealedBytes = 256 * 1024;
    private readonly SemaphoreSlim gate = new(1, 1);
    public bool Persistent => true;
    public static string DefaultFolder() => Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Pigeonpost", "room-creations");
    private string FolderFor(MessageRecoveryScope scope) => Path.Combine(folder,
        RoomCreationAttempt.Hash(scope.OwnerKey + "\0" + scope.Origin)[..32], RoomCreationAttempt.Hash(scope.Mailbox)[..32]);
    private string PathFor(MessageRecoveryScope scope) => Path.Combine(FolderFor(scope), "room.pending");

    public async Task<RoomCreationAttempt?> LoadAsync(MessageRecoveryScope scope, CancellationToken ct = default)
    {
        await gate.WaitAsync(ct).ConfigureAwait(false);
        try { scope.RequireCurrent(ct); return await Read(scope, ct).ConfigureAwait(false); }
        catch (Exception ex) when (Wrap(ex)) { throw new RoomCreationUnavailableException(); }
        finally { gate.Release(); }
    }
    public async Task SaveAsync(MessageRecoveryScope scope, RoomCreationAttempt attempt, CancellationToken ct = default)
    {
        attempt.RequireScope(scope);
        await gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            var existing = await Read(scope, ct).ConfigureAwait(false);
            if (existing is not null && !existing.SameRequest(attempt)) throw new RoomCreationConflictException();
            await Write(scope, attempt, ct).ConfigureAwait(false);
        }
        catch (Exception ex) when (Wrap(ex)) { throw new RoomCreationUnavailableException(); }
        finally { gate.Release(); }
    }
    public async Task<bool> RemoveAsync(MessageRecoveryScope scope, RoomCreationAttempt expected, CancellationToken ct = default)
    {
        expected.RequireScope(scope);
        await gate.WaitAsync(ct).ConfigureAwait(false);
        try
        {
            var existing = await Read(scope, ct).ConfigureAwait(false);
            if (existing is null) return false;
            if (!existing.SameRequest(expected)) throw new RoomCreationConflictException();
            return scope.Local(() => { ct.ThrowIfCancellationRequested(); File.Delete(PathFor(scope)); return true; });
        }
        catch (Exception ex) when (Wrap(ex)) { throw new RoomCreationUnavailableException(); }
        finally { gate.Release(); }
    }
    private async Task<RoomCreationAttempt?> Read(MessageRecoveryScope scope, CancellationToken ct)
    {
        scope.RequireCurrent(ct);
        if (!scope.Local(() => File.Exists(PathFor(scope)))) return null;
        byte[] blob;
        await using (var input = scope.Local(() => new FileStream(PathFor(scope), FileMode.Open, FileAccess.Read, FileShare.Read, 8192, FileOptions.Asynchronous)))
        {
            if (input.Length <= Magic.Length || input.Length > MaxSealedBytes) throw new RoomCreationUnavailableException();
            blob = new byte[checked((int)input.Length)];
            await input.ReadExactlyAsync(blob, ct).ConfigureAwait(false);
        }
        scope.RequireCurrent(ct);
        if (!blob.AsSpan(0, Magic.Length).SequenceEqual(Magic)) throw new RoomCreationUnavailableException();
        var plain = await protector.UnprotectAsync(blob[Magic.Length..]).ConfigureAwait(false);
        try
        {
            scope.RequireCurrent(ct);
            if (plain.Length > MaxPlainBytes) throw new RoomCreationUnavailableException();
            var entry = JsonSerializer.Deserialize<Envelope>(plain, PostboxJson.Options) ?? throw new RoomCreationUnavailableException();
            if (entry.Version != 1 || entry.OwnerKey != scope.OwnerKey || entry.Origin != scope.Origin || entry.Mailbox != scope.Mailbox
                || entry.Encoded is null || entry.EncodedHash != RoomCreationAttempt.Hash(entry.Encoded)) throw new RoomCreationUnavailableException();
            return new RoomCreationAttempt(scope, OrganizationRoomCreate.Restore(entry.Encoded), entry.CreatedAt, entry.CandidateId);
        }
        finally { CryptographicOperations.ZeroMemory(plain); }
    }
    private async Task Write(MessageRecoveryScope scope, RoomCreationAttempt attempt, CancellationToken ct)
    {
        var plain = JsonSerializer.SerializeToUtf8Bytes(new Envelope(1, scope.OwnerKey, scope.Origin, scope.Mailbox, attempt.CreatedAt,
            attempt.Payload.Encoded, RoomCreationAttempt.Hash(attempt.Payload.Encoded), attempt.CandidateId), PostboxJson.Options);
        string? staging = null;
        try
        {
            if (plain.Length > MaxPlainBytes) throw new RoomCreationUnavailableException();
            var sealedBytes = await protector.ProtectAsync(plain).ConfigureAwait(false);
            scope.RequireCurrent(ct);
            if (sealedBytes.Length > MaxSealedBytes - Magic.Length) throw new RoomCreationUnavailableException();
            scope.Local(() => Directory.CreateDirectory(FolderFor(scope)));
            staging = PathFor(scope) + "." + Guid.NewGuid().ToString("N") + ".tmp";
            await using (var output = scope.Local(() => new FileStream(staging, FileMode.CreateNew, FileAccess.Write, FileShare.None, 8192, FileOptions.Asynchronous | FileOptions.WriteThrough)))
            {
                await output.WriteAsync(Magic, ct).ConfigureAwait(false); await output.WriteAsync(sealedBytes, ct).ConfigureAwait(false);
                await output.FlushAsync(ct).ConfigureAwait(false); output.Flush(flushToDisk: true);
            }
            scope.Local(() => { ct.ThrowIfCancellationRequested(); File.Move(staging, PathFor(scope), overwrite: true); return true; }); staging = null;
        }
        finally
        {
            CryptographicOperations.ZeroMemory(plain);
            if (staging is not null) { try { File.Delete(staging); } catch (IOException) { } catch (UnauthorizedAccessException) { } }
        }
    }
    private static bool Wrap(Exception error) => error is IOException or UnauthorizedAccessException or CryptographicException or JsonException or ArgumentException
        && error is not RoomCreationUnavailableException;
    private sealed record Envelope(int Version, string OwnerKey, string Origin, string Mailbox, long CreatedAt, string Encoded, string EncodedHash, string? CandidateId);
}
