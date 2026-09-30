using System.Text.Json;
using System.Text.Json.Serialization;

namespace Pigeonpost.Core;

public static class PostboxJson
{
    public static JsonSerializerOptions Options { get; } = new()
    {
        PropertyNamingPolicy = JsonNamingPolicy.SnakeCaseLower,
        DefaultIgnoreCondition = JsonIgnoreCondition.WhenWritingNull,
        RespectNullableAnnotations = true
    };
}

public sealed record Mailbox(string Address, string? Handle = null, string? Label = null)
{
    public string Key => Handle ?? Address;
    public string DisplayName => Handle is { } handle && handle.EndsWith("/main", StringComparison.Ordinal)
        ? PostAddress.DisplayName(handle) : Label ?? Handle ?? Address;
}

public static class PostAddress
{
    // The whole handle, with a trailing "/main" dropped.
    //
    // Showing the last segment alone made every namespace's default inbox read as the same word,
    // "main", so the one name on screen identified nobody. The rest of a handle is not decoration
    // either: /wodo/home and /bekir/home are different mailboxes, and a row saying only "home" does
    // not say whose. The namespace always survives; only the "main" that a bare /<namespace>
    // already resolves to is worth dropping. Matches PeerFace.displayName in the Apple client.
    public static string DisplayName(string peer)
    {
        if (peer.StartsWith("/k/", StringComparison.Ordinal)) return peer;
        var parts = peer.Split('/', StringSplitOptions.RemoveEmptyEntries);
        if (parts.Length == 0) return peer;
        if (parts.Length > 1 && parts[^1] == "main") return "/" + string.Join('/', parts[..^1]);
        return "/" + string.Join('/', parts);
    }

    public static string Input(string value)
    {
        value = value.Trim();
        return value.StartsWith('/') ? value : "/" + value;
    }

    // Typing guard; the server applies the full address grammar and routing rules.
    public static bool IsValid(string peer)
    {
        if (peer.Length is < 2 or > 512 || !peer.StartsWith('/')) return false;
        var parts = peer[1..].Split('/');
        return parts.All(part => part.Length > 0 && part is not "." and not ".."
            && (!part.Contains('*') || part.Contains('@'))
            && part.All(c => char.IsAsciiLetterOrDigit(c) || "!$&'*+-=^_`{|}~.@".Contains(c)));
    }
}

// Optional fields preserve compatibility with older postboxes. Only server fields describe trust.
public sealed record InboxMessage
{
    public required string MessageId { get; init; }
    public required string Body { get; init; }
    public string? From { get; init; }
    public string? To { get; init; }
    public string? Direction { get; init; }
    public string? Peer { get; init; }
    public string? PeerHandle { get; init; }
    public string? SenderHandle { get; init; }
    public string? ThreadId { get; init; }
    public long? ReceivedAt { get; init; }
    public long? SentAt { get; init; }
    public bool? Read { get; init; }
    public string? Autonomy { get; init; }
    public string? Verb { get; init; }
    public string? HeldBecause { get; init; }
    public IReadOnlyList<MessageAttachment>? Attachments { get; init; }

    [JsonIgnore] public bool IsOutgoing => Direction == "out";
    [JsonIgnore] public long At => IsOutgoing ? SentAt ?? ReceivedAt ?? 0 : ReceivedAt ?? SentAt ?? 0;
    [JsonIgnore] public string PeerKey => PeerHandle ?? Peer ?? (IsOutgoing ? To : SenderHandle ?? From) ?? "unknown";
}

public sealed record MessageAttachment(string Id, string Filename, string MediaType, long Bytes);
public sealed record Contact(string Peer, string? Alias, string Admission, string Autonomy, IReadOnlyList<string>? AllowedVerbs = null)
{
    [JsonIgnore] public bool IsWildcard => Peer.EndsWith("/*", StringComparison.Ordinal);
}
public sealed record ServerThread(string ThreadId, string Peer, string? Title = null, bool? IsDefault = null,
    long? CreatedAt = null, long? LastAt = null, bool? Archived = null);
public sealed record SendReceipt(string? MessageId, string? SentCopyId);
public sealed record InboxSnapshot(IReadOnlyList<InboxMessage> Messages, IReadOnlyList<ServerThread> Threads,
    IReadOnlyList<Contact> Contacts, IReadOnlySet<string> Archived)
{
    public static InboxSnapshot Empty { get; } = new([], [], [], new HashSet<string>(StringComparer.Ordinal));
}

public enum DeliveryStatus { Sent, Sending, Failed }

public sealed record PendingMessage(string Id, string Mailbox, string To, string Body, long At,
    string? ThreadId, DeliveryStatus Status = DeliveryStatus.Sending, string? SentCopyId = null);

public sealed record ThreadMessage(string Id, string Body, long At, string? ThreadId, bool IsOutgoing,
    bool Read = true, string? Autonomy = null, string? Verb = null, string? HeldBecause = null,
    DeliveryStatus Status = DeliveryStatus.Sent, IReadOnlyList<MessageAttachment>? Attachments = null)
{
    public bool IsHeld => !IsOutgoing && Autonomy == "review" && !string.IsNullOrEmpty(Verb);
    public string DisplayBody => RequestEnvelope.DisplayText(Body);
    public string AttachmentSummary => string.Join(" · ", (Attachments ?? []).Select(a => a.Filename));
    public string StatusLabel => Status switch
    {
        DeliveryStatus.Sending => "Sending…",
        DeliveryStatus.Failed => "Delivery not confirmed",
        _ when IsHeld => "Held for review",
        _ when !IsOutgoing && Autonomy == "auto" => "Allowed by recipient",
        _ => ""
    };
    public string TimeLabel => At is >= -62135596800 and <= 253402300799
        ? DateTimeOffset.FromUnixTimeSeconds(At).ToLocalTime().ToString("g") : "";
}

public sealed record Conversation(string Peer, string Name, IReadOnlyList<ThreadMessage> Messages,
    bool IsMine, Contact? Contact)
{
    public int Unread => Messages.Count(m => !m.IsOutgoing && !m.Read);
    public int Held => Messages.Count(m => m.IsHeld);
    public long Last => Messages.LastOrDefault()?.At ?? 0;
    public string Preview => Messages.LastOrDefault() is { } m ? m.DisplayBody.ReplaceLineEndings(" ") : "Start a conversation";
    public string Badge => string.Join(" · ", new[] { Unread > 0 ? $"{Unread} unread" : "", Held > 0 ? $"{Held} held" : "" }.Where(s => s.Length > 0));
}

public sealed record Subject(string Id, string? Title, bool IsDefault, IReadOnlyList<ThreadMessage> Messages, long Last)
{
    public string Name => !string.IsNullOrWhiteSpace(Title) ? Title : IsDefault ? "General" : "Untitled";
    public int Unread => Messages.Count(m => !m.IsOutgoing && !m.Read);
    public string Detail => $"{Messages.Count} message{(Messages.Count == 1 ? "" : "s")}" + (Unread > 0 ? $" · {Unread} unread" : "");
}

public static class RequestEnvelope
{
    // Matches the current Apple composer. The recipient still owns every permission decision.
    public static string Work(string text) => JsonSerializer.Serialize(new
    {
        v = 1, verb = "full_access", args = new { task = text }, note = text
    });

    public static string DisplayText(string body)
    {
        if (!body.StartsWith('{')) return body;
        try
        {
            using var doc = JsonDocument.Parse(body);
            var root = doc.RootElement;
            if (root.ValueKind != JsonValueKind.Object || !root.TryGetProperty("v", out var version)
                || version.ValueKind != JsonValueKind.Number || !version.TryGetInt32(out var v) || v != 1
                || !root.TryGetProperty("verb", out var verb) || verb.ValueKind != JsonValueKind.String) return body;
            if (root.TryGetProperty("args", out var args) && args.ValueKind == JsonValueKind.Object)
                foreach (var key in new[] { "task", "question" })
                    if (args.TryGetProperty(key, out var text) && text.ValueKind == JsonValueKind.String)
                        return text.GetString() ?? body;
            return root.TryGetProperty("note", out var note) && note.ValueKind == JsonValueKind.String
                ? note.GetString() ?? body : body;
        }
        catch (JsonException) { return body; }
    }
}
