using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using Pigeonpost.Core.Groups;

namespace Pigeonpost.Core;

public sealed class MessageAttemptUnavailableException() : IOException("Pending delivery could not be read or saved. Try again before sending another message.");
public sealed class MessageAttemptConflictException() : IOException("Resolve the previous pending delivery before sending another message in this subject.");

public sealed class PreparedMessageAttempt
{
    private const int MaxWireBytes = 2 * 1024 * 1024;
    public MessageSendRequest Request { get; }
    public long CreatedAt { get; }
    internal string OwnerKey { get; }
    internal string Origin { get; }
    internal string Mailbox { get; }
    internal string Slot { get; }
    internal string WireJson { get; }
    private PreparedMessageAttempt(MessageRecoveryScope scope, MessageSendRequest request, string wireJson, long createdAt)
    {
        scope.RequireCurrent();
        if (request.From != scope.Mailbox || !MessageRecoveryScope.CanonicalMailbox(request.To) || request.Body is null
            || Encoding.UTF8.GetByteCount(wireJson) > MaxWireBytes) throw new ArgumentException("Verify the exact sender and recipient before preparing delivery.");
        OwnerKey = scope.OwnerKey; Origin = scope.Origin; Mailbox = scope.Mailbox;
        Request = request; Slot = SlotFor(request.To, request.ThreadId); WireJson = wireJson; CreatedAt = createdAt;
    }
    public static PreparedMessageAttempt Create(MessageRecoveryScope scope, MessageSendRequest request) =>
        new(scope, request, JsonSerializer.Serialize(request, PostboxJson.Options), DateTimeOffset.UtcNow.ToUnixTimeSeconds());
    internal static string SlotFor(string recipient, string? thread) => Hash(recipient + "\0" + (thread ?? ""));
    internal static string Hash(string value) => Convert.ToHexString(SHA256.HashData(Encoding.UTF8.GetBytes(value))).ToLowerInvariant();
    internal void RequireScope(MessageRecoveryScope scope)
    {
        scope.RequireCurrent();
        if (OwnerKey != scope.OwnerKey || Origin != scope.Origin || Mailbox != scope.Mailbox) throw new AuthorizationRetiredException();
    }
    internal byte[] Encode(MessageRecoveryScope scope)
    {
        RequireScope(scope);
        return JsonSerializer.SerializeToUtf8Bytes(new Envelope(1, OwnerKey, Origin, Mailbox, Slot, CreatedAt, WireJson, Hash(WireJson)), PostboxJson.Options);
    }
    internal static PreparedMessageAttempt Decode(MessageRecoveryScope scope, byte[] bytes, string expectedSlot)
    {
        scope.RequireCurrent();
        var envelope = JsonSerializer.Deserialize<Envelope>(bytes, PostboxJson.Options) ?? throw new MessageAttemptUnavailableException();
        if (envelope.Version != 1 || envelope.OwnerKey != scope.OwnerKey || envelope.Origin != scope.Origin
            || envelope.Mailbox != scope.Mailbox || envelope.Slot != expectedSlot || envelope.WireJson is null
            || envelope.WireSha256 != Hash(envelope.WireJson) || envelope.CreatedAt is < 0 or > 253402300799)
            throw new MessageAttemptUnavailableException();
        var request = ParseRequest(envelope.WireJson);
        var result = new PreparedMessageAttempt(scope, request, envelope.WireJson, envelope.CreatedAt);
        if (result.Slot != expectedSlot) throw new MessageAttemptUnavailableException();
        return result;
    }
    private static MessageSendRequest ParseRequest(string json)
    {
        if (Encoding.UTF8.GetByteCount(json) > MaxWireBytes) throw new MessageAttemptUnavailableException();
        using var doc = JsonDocument.Parse(json);
        var root = doc.RootElement;
        string[] names = ["from", "to", "body", "thread_id", "attachments", "msg_ref", "reply_to", "quote", "edit_of"];
        if (root.ValueKind != JsonValueKind.Object || root.EnumerateObject().Any(p => !names.Contains(p.Name, StringComparer.Ordinal))
            || root.EnumerateObject().Select(p => p.Name).Distinct(StringComparer.Ordinal).Count() != root.EnumerateObject().Count())
            throw new MessageAttemptUnavailableException();
        string? Text(string name) => !root.TryGetProperty(name, out var value) || value.ValueKind == JsonValueKind.Null ? null : value.GetString();
        var files = root.TryGetProperty("attachments", out var attachments) && attachments.ValueKind != JsonValueKind.Null
            ? attachments.EnumerateArray().Select(v => v.GetString() ?? throw new MessageAttemptUnavailableException()).ToArray() : null;
        return new(Text("from") ?? throw new MessageAttemptUnavailableException(), Text("to") ?? throw new MessageAttemptUnavailableException(),
            Text("body") ?? throw new MessageAttemptUnavailableException(), Text("thread_id"), files,
            Text("msg_ref") ?? throw new MessageAttemptUnavailableException(), Text("reply_to"), Text("quote"), Text("edit_of"));
    }
    // Validated again after decrypt; a file's path never authorizes its contents.
    private sealed record Envelope(int Version, string OwnerKey, string Origin, string Mailbox, string Slot, long CreatedAt, string WireJson, string WireSha256)
    { public override string ToString() => "MessageAttemptEnvelope(redacted)"; }
    public override string ToString() => "PreparedMessageAttempt(redacted)";
}

public interface IMessageAttemptStore
{
    bool Persistent { get; }
    Task<PreparedMessageAttempt?> LoadAsync(MessageRecoveryScope scope, string canonicalRecipient, string? threadId, CancellationToken cancellationToken = default);
    Task SaveAsync(MessageRecoveryScope scope, PreparedMessageAttempt attempt, CancellationToken cancellationToken = default);
    Task<bool> RemoveAsync(MessageRecoveryScope scope, string canonicalRecipient, string? threadId, string expectedReference, CancellationToken cancellationToken = default);
}

public sealed class FileMessageAttemptStore(string folder, IStateProtector protector) : IMessageAttemptStore
{
    private static readonly byte[] Magic = "PPMA1"u8.ToArray();
    private const int MaxPlainBytes = 4 * 1024 * 1024, MaxSealedBytes = 8 * 1024 * 1024;
    private readonly SemaphoreSlim gate = new(1, 1);
    public bool Persistent => true;
    private static string Slot(MessageRecoveryScope scope, string recipient, string? thread)
    {
        scope.RequireCurrent();
        if (!MessageRecoveryScope.CanonicalMailbox(recipient)) throw new ArgumentException("Verify the recipient inbox before recovering delivery.");
        return PreparedMessageAttempt.SlotFor(recipient, thread);
    }
    private string FolderFor(MessageRecoveryScope scope) => Path.Combine(folder,
        PreparedMessageAttempt.Hash(scope.OwnerKey + "\0" + scope.Origin)[..32], PreparedMessageAttempt.Hash(scope.Mailbox)[..32]);
    private string PathFor(MessageRecoveryScope scope, string slot) => Path.Combine(FolderFor(scope), slot[..32] + ".pending");
    private FileStream Lock(MessageRecoveryScope scope, string slot) => scope.Local(() =>
    {
        Directory.CreateDirectory(FolderFor(scope));
        return new FileStream(PathFor(scope, slot) + ".lock", FileMode.OpenOrCreate, FileAccess.ReadWrite, FileShare.None);
    });

    public async Task<PreparedMessageAttempt?> LoadAsync(MessageRecoveryScope scope, string canonicalRecipient, string? threadId, CancellationToken cancellationToken = default)
    {
        var slot = Slot(scope, canonicalRecipient, threadId);
        await gate.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            scope.RequireCurrent(cancellationToken);
            if (!scope.Local(() => File.Exists(PathFor(scope, slot)))) return null;
            using var localLock = Lock(scope, slot);
            return await Read(scope, slot, cancellationToken).ConfigureAwait(false);
        }
        catch (Exception ex) when (Wrap(ex)) { throw new MessageAttemptUnavailableException(); }
        finally { gate.Release(); }
    }

    public async Task SaveAsync(MessageRecoveryScope scope, PreparedMessageAttempt attempt, CancellationToken cancellationToken = default)
    {
        attempt.RequireScope(scope);
        await gate.WaitAsync(cancellationToken).ConfigureAwait(false);
        string? staging = null;
        byte[]? plain = null;
        try
        {
            scope.RequireCurrent(cancellationToken);
            using var localLock = Lock(scope, attempt.Slot);
            var existing = await Read(scope, attempt.Slot, cancellationToken).ConfigureAwait(false);
            if (existing is not null)
            {
                if (existing.WireJson == attempt.WireJson) return;
                throw new MessageAttemptConflictException();
            }
            plain = attempt.Encode(scope);
            if (plain.Length > MaxPlainBytes) throw new MessageAttemptUnavailableException();
            var sealedBytes = await protector.ProtectAsync(plain).ConfigureAwait(false);
            scope.RequireCurrent(cancellationToken);
            if (sealedBytes.Length > MaxSealedBytes - Magic.Length) throw new MessageAttemptUnavailableException();
            staging = PathFor(scope, attempt.Slot) + "." + Guid.NewGuid().ToString("N") + ".tmp";
            await using (var output = new FileStream(staging, FileMode.CreateNew, FileAccess.Write, FileShare.None, 8192, FileOptions.Asynchronous | FileOptions.WriteThrough))
            {
                await output.WriteAsync(Magic, cancellationToken).ConfigureAwait(false);
                await output.WriteAsync(sealedBytes, cancellationToken).ConfigureAwait(false);
                await output.FlushAsync(cancellationToken).ConfigureAwait(false);
                output.Flush(flushToDisk: true);
            }
            scope.RequireCurrent(cancellationToken);
            scope.Local(() => { cancellationToken.ThrowIfCancellationRequested(); File.Move(staging, PathFor(scope, attempt.Slot), overwrite: false); return true; });
            staging = null;
        }
        catch (Exception ex) when (Wrap(ex)) { throw new MessageAttemptUnavailableException(); }
        finally
        {
            if (plain is not null) CryptographicOperations.ZeroMemory(plain);
            if (staging is not null) { try { File.Delete(staging); } catch (IOException) { } catch (UnauthorizedAccessException) { } }
            gate.Release();
        }
    }

    public async Task<bool> RemoveAsync(MessageRecoveryScope scope, string canonicalRecipient, string? threadId, string expectedReference, CancellationToken cancellationToken = default)
    {
        var slot = Slot(scope, canonicalRecipient, threadId);
        if (!MessageReferences.IsValid(expectedReference)) throw new ArgumentException("Invalid pending delivery reference.");
        await gate.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            scope.RequireCurrent(cancellationToken);
            if (!scope.Local(() => File.Exists(PathFor(scope, slot)))) return false;
            using var localLock = Lock(scope, slot);
            var existing = await Read(scope, slot, cancellationToken).ConfigureAwait(false);
            if (existing is null) return false;
            if (existing.Request.MsgRef != expectedReference) throw new MessageAttemptConflictException();
            return scope.Local(() => { cancellationToken.ThrowIfCancellationRequested(); File.Delete(PathFor(scope, slot)); return true; });
        }
        catch (Exception ex) when (Wrap(ex)) { throw new MessageAttemptUnavailableException(); }
        finally { gate.Release(); }
    }

    private async Task<PreparedMessageAttempt?> Read(MessageRecoveryScope scope, string slot, CancellationToken ct)
    {
        scope.RequireCurrent(ct);
        var path = PathFor(scope, slot);
        var bytes = scope.Local(() =>
        {
            if (!File.Exists(path)) return null;
            using var input = new FileStream(path, FileMode.Open, FileAccess.Read, FileShare.Read);
            if (input.Length < Magic.Length || input.Length > MaxSealedBytes) throw new MessageAttemptUnavailableException();
            var result = new byte[(int)input.Length]; input.ReadExactly(result);
            if (input.ReadByte() != -1) throw new MessageAttemptUnavailableException();
            return result;
        });
        if (bytes is null) return null;
        if (!bytes.AsSpan(0, Magic.Length).SequenceEqual(Magic)) throw new MessageAttemptUnavailableException();
        byte[]? plain = null;
        try
        {
            plain = await protector.UnprotectAsync(bytes[Magic.Length..]).ConfigureAwait(false);
            scope.RequireCurrent(ct);
            if (plain.Length > MaxPlainBytes) throw new MessageAttemptUnavailableException();
            return PreparedMessageAttempt.Decode(scope, plain, slot);
        }
        finally { if (plain is not null) CryptographicOperations.ZeroMemory(plain); }
    }
    private static bool Wrap(Exception ex) => ex is not OperationCanceledException and not MessageAttemptConflictException and not MessageAttemptUnavailableException;
}
