using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Pigeonpost.Core.Groups;

namespace Pigeonpost.Core.Calendar;

public enum CalendarAttemptKind { Calendar, Event }
public sealed class CalendarAttemptUnavailableException() : IOException("Saved calendar changes could not be opened. Review them before creating another item.");
public sealed class CalendarAttemptConflictException() : InvalidOperationException("Review the pending calendar change before creating another item of this kind.");
public sealed class CalendarAttemptExpiredException() : InvalidOperationException("This saved calendar change is too old to retry safely. Review the calendar before discarding recovery.");

public sealed class PreparedCalendarAttempt
{
    public CalendarAttemptKind Kind { get; }
    public CalendarWritePayload Payload { get; }
    public string Id { get; }
    public long CreatedAt { get; }
    internal string OwnerKey { get; }
    internal string Origin { get; }
    public string Mailbox { get; }
    internal PreparedCalendarAttempt(MessageRecoveryScope scope, CalendarAttemptKind kind, CalendarWritePayload payload, long createdAt)
    {
        scope.RequireCurrent();
        if (!Enum.IsDefined(kind) || createdAt is < 0 or > 253402300799) throw new ArgumentException("This calendar attempt is invalid.");
        Kind = kind; Payload = payload; Id = payload.RequestKey(); CreatedAt = createdAt;
        OwnerKey = scope.OwnerKey; Origin = scope.Origin; Mailbox = scope.Mailbox;
    }
    public bool CanRetry(long now)
    { try { var age = checked(now - CreatedAt); return age is >= 0 and <= 86400; } catch (OverflowException) { return false; } }
    internal void RequireScope(MessageRecoveryScope scope)
    {
        scope.RequireCurrent();
        if (OwnerKey != scope.OwnerKey || Origin != scope.Origin || Mailbox != scope.Mailbox) throw new AuthorizationRetiredException();
    }
    internal bool Same(PreparedCalendarAttempt other) => Kind == other.Kind && Id == other.Id && CreatedAt == other.CreatedAt
        && OwnerKey == other.OwnerKey && Origin == other.Origin && Mailbox == other.Mailbox && Payload.WireJson == other.Payload.WireJson;
    internal static string Hash(string text) => Convert.ToHexStringLower(SHA256.HashData(Encoding.UTF8.GetBytes(text)));
    public override string ToString() => "PreparedCalendarAttempt(redacted)";
}

public interface ICalendarAttemptStore
{
    bool Persistent { get; }
    Task<IReadOnlyList<PreparedCalendarAttempt>> ListAsync(MessageRecoveryScope scope, CancellationToken cancellationToken = default);
    Task SaveAsync(MessageRecoveryScope scope, PreparedCalendarAttempt attempt, CancellationToken cancellationToken = default);
    Task<bool> RemoveAsync(MessageRecoveryScope scope, PreparedCalendarAttempt expected, CancellationToken cancellationToken = default);
}

// The same server-authorized canonical mailbox scope used by protected messages is reused here;
// the envelope, prefix and files are separate. A login owner hint alone never opens this state.
public sealed class FileCalendarAttemptStore(string folder, IStateProtector protector) : ICalendarAttemptStore
{
    private static readonly byte[] Magic = "PPCA1"u8.ToArray();
    private const int MaxPlainBytes = 4 * 1024 * 1024, MaxSealedBytes = 8 * 1024 * 1024;
    private readonly SemaphoreSlim gate = new(1, 1);
    public bool Persistent => true;
    private string FolderFor(MessageRecoveryScope scope) => Path.Combine(folder,
        PreparedCalendarAttempt.Hash(scope.OwnerKey + "\0" + scope.Origin)[..32], PreparedCalendarAttempt.Hash(scope.Mailbox)[..32]);
    private string PathFor(MessageRecoveryScope scope) => Path.Combine(FolderFor(scope), "calendar.pending");
    private FileStream Lock(MessageRecoveryScope scope) => scope.Local(() =>
    {
        Directory.CreateDirectory(FolderFor(scope));
        return new FileStream(Path.Combine(FolderFor(scope), "calendar.lock"), FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None);
    });
    public async Task<IReadOnlyList<PreparedCalendarAttempt>> ListAsync(MessageRecoveryScope scope, CancellationToken cancellationToken = default)
    {
        await gate.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            scope.RequireCurrent(cancellationToken);
            if (!scope.Local(() => Directory.Exists(FolderFor(scope)))) return [];
            using var fileLock = Lock(scope);
            return await Read(scope, cancellationToken).ConfigureAwait(false);
        }
        catch (Exception ex) when (Wrap(ex)) { throw new CalendarAttemptUnavailableException(); }
        finally { gate.Release(); }
    }
    public async Task SaveAsync(MessageRecoveryScope scope, PreparedCalendarAttempt attempt, CancellationToken cancellationToken = default)
    {
        attempt.RequireScope(scope);
        await gate.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            scope.RequireCurrent(cancellationToken);
            using var fileLock = Lock(scope);
            var entries = (await Read(scope, cancellationToken).ConfigureAwait(false)).ToList();
            var existing = entries.FirstOrDefault(e => e.Kind == attempt.Kind || e.Id == attempt.Id);
            if (existing is not null)
            { if (existing.Same(attempt)) return; throw new CalendarAttemptConflictException(); }
            entries.Add(attempt);
            await Write(scope, entries, cancellationToken).ConfigureAwait(false);
        }
        catch (Exception ex) when (Wrap(ex)) { throw new CalendarAttemptUnavailableException(); }
        finally { gate.Release(); }
    }
    public async Task<bool> RemoveAsync(MessageRecoveryScope scope, PreparedCalendarAttempt expected, CancellationToken cancellationToken = default)
    {
        expected.RequireScope(scope);
        await gate.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            scope.RequireCurrent(cancellationToken);
            if (!scope.Local(() => Directory.Exists(FolderFor(scope)))) return false;
            using var fileLock = Lock(scope);
            var entries = (await Read(scope, cancellationToken).ConfigureAwait(false)).ToList();
            var existing = entries.FirstOrDefault(e => e.Kind == expected.Kind);
            if (existing is null) return false;
            if (!existing.Same(expected)) throw new CalendarAttemptConflictException();
            entries.Remove(existing);
            if (entries.Count == 0) scope.Local(() => { cancellationToken.ThrowIfCancellationRequested(); File.Delete(PathFor(scope)); return true; });
            else await Write(scope, entries, cancellationToken).ConfigureAwait(false);
            return true;
        }
        catch (Exception ex) when (Wrap(ex)) { throw new CalendarAttemptUnavailableException(); }
        finally { gate.Release(); }
    }
    private async Task<IReadOnlyList<PreparedCalendarAttempt>> Read(MessageRecoveryScope scope, CancellationToken ct)
    {
        scope.RequireCurrent(ct);
        if (!scope.Local(() => File.Exists(PathFor(scope)))) return [];
        byte[] blob;
        await using (var input = scope.Local(() => new FileStream(PathFor(scope), FileMode.Open, FileAccess.Read, FileShare.Read, 8192, FileOptions.Asynchronous)))
        {
            if (input.Length <= Magic.Length || input.Length > MaxSealedBytes) throw new CalendarAttemptUnavailableException();
            blob = new byte[checked((int)input.Length)];
            await input.ReadExactlyAsync(blob, ct).ConfigureAwait(false);
            if (input.ReadByte() != -1) throw new CalendarAttemptUnavailableException();
        }
        scope.RequireCurrent(ct);
        if (!blob.AsSpan(0, Magic.Length).SequenceEqual(Magic)) throw new CalendarAttemptUnavailableException();
        var plain = await protector.UnprotectAsync(blob[Magic.Length..]).ConfigureAwait(false);
        try
        {
            scope.RequireCurrent(ct);
            if (plain.Length > MaxPlainBytes) throw new CalendarAttemptUnavailableException();
            var envelope = JsonSerializer.Deserialize<Envelope>(plain, PostboxJson.Options) ?? throw new CalendarAttemptUnavailableException();
            if (envelope.Version != 1 || envelope.OwnerKey != scope.OwnerKey || envelope.Origin != scope.Origin || envelope.Mailbox != scope.Mailbox
                || envelope.Entries is null || envelope.Entries.Count is < 1 or > 2) throw new CalendarAttemptUnavailableException();
            var result = new List<PreparedCalendarAttempt>();
            foreach (var entry in envelope.Entries)
            {
                if (entry is null || entry.Encoded is null || entry.EncodedHash != PreparedCalendarAttempt.Hash(entry.Encoded)) throw new CalendarAttemptUnavailableException();
                var kind = entry.Kind switch { "calendar" => CalendarAttemptKind.Calendar, "event" => CalendarAttemptKind.Event, _ => throw new CalendarAttemptUnavailableException() };
                var attempt = new PreparedCalendarAttempt(scope, kind, CalendarWritePayload.Restore(entry.Encoded), entry.CreatedAt);
                if (entry.Id != attempt.Id) throw new CalendarAttemptUnavailableException(); result.Add(attempt);
            }
            Validate(scope, result); scope.RequireCurrent(ct); return result.ToArray();
        }
        finally { CryptographicOperations.ZeroMemory(plain); }
    }
    private static void Validate(MessageRecoveryScope scope, IReadOnlyList<PreparedCalendarAttempt> entries)
    {
        if (entries.Count > 2 || entries.Select(e => e.Kind).Distinct().Count() != entries.Count
            || entries.Select(e => e.Id).Distinct(StringComparer.Ordinal).Count() != entries.Count) throw new CalendarAttemptUnavailableException();
        foreach (var entry in entries) entry.RequireScope(scope);
    }
    private async Task Write(MessageRecoveryScope scope, IReadOnlyList<PreparedCalendarAttempt> entries, CancellationToken ct)
    {
        Validate(scope, entries);
        var plain = JsonSerializer.SerializeToUtf8Bytes(new Envelope(1, scope.OwnerKey, scope.Origin, scope.Mailbox,
            entries.Select(e => new Entry(e.Kind == CalendarAttemptKind.Calendar ? "calendar" : "event", e.Id, e.CreatedAt,
                e.Payload.WireJson, PreparedCalendarAttempt.Hash(e.Payload.WireJson))).ToArray()), PostboxJson.Options);
        string? staging = null;
        try
        {
            if (plain.Length > MaxPlainBytes) throw new CalendarAttemptUnavailableException();
            var sealedBytes = await protector.ProtectAsync(plain).ConfigureAwait(false);
            scope.RequireCurrent(ct);
            if (sealedBytes.Length > MaxSealedBytes - Magic.Length) throw new CalendarAttemptUnavailableException();
            staging = PathFor(scope) + "." + Guid.NewGuid().ToString("N") + ".tmp";
            await using (var output = scope.Local(() => new FileStream(staging, FileMode.CreateNew, FileAccess.Write, FileShare.None, 8192, FileOptions.Asynchronous | FileOptions.WriteThrough)))
            {
                await output.WriteAsync(Magic, ct).ConfigureAwait(false); await output.WriteAsync(sealedBytes, ct).ConfigureAwait(false);
                await output.FlushAsync(ct).ConfigureAwait(false); output.Flush(flushToDisk: true);
            }
            scope.RequireCurrent(ct);
            scope.Local(() => { ct.ThrowIfCancellationRequested(); File.Move(staging, PathFor(scope), overwrite: true); return true; }); staging = null;
        }
        finally
        {
            CryptographicOperations.ZeroMemory(plain);
            if (staging is not null) { try { File.Delete(staging); } catch (IOException) { } catch (UnauthorizedAccessException) { } }
        }
    }
    private static bool Wrap(Exception error) => error is IOException or UnauthorizedAccessException or CryptographicException or JsonException or ArgumentException
        && error is not CalendarAttemptUnavailableException;
    private sealed record Envelope(int Version, string OwnerKey, string Origin, string Mailbox, IReadOnlyList<Entry>? Entries);
    private sealed record Entry(string Kind, string Id, long CreatedAt, string Encoded, string EncodedHash);
}
