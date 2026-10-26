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
    public string DisplayName => Handle is { } handle ? PostAddress.DisplayName(handle) : Label ?? Address;
}

public static class PostAddress
{
    // Short segments that read as acronyms. The same list as ACRONYMS in site-inbox/names.js.
    public static readonly string[] Acronyms = ["hr", "it", "qa", "ui", "ux", "pr", "ai", "ceo", "cto", "cfo", "ops", "dev"];

    // A handle as a person's name: /bekir/main and /bekir are "Bekir", /wodo/hr is "Wodo HR",
    // /acme/john-doe is "Acme John Doe". Every segment survives (/wodo/home and /bekir/home are
    // different mailboxes); only the "main" a bare /<namespace> resolves to is dropped. Key
    // addresses, address-style handles and GitHub logins keep their old display, and anything that is
    // not a handle is returned unchanged. A contact alias wins over this. Matches PeerFace.displayName.
    public static string DisplayName(string peer)
    {
        if (peer.StartsWith("/k/", StringComparison.Ordinal) || !peer.StartsWith('/')) return peer;
        var parts = peer.Split('/', StringSplitOptions.RemoveEmptyEntries).ToList();
        if (parts.Count == 0) return peer;
        if (parts.Count > 1 && parts[^1] == "main") parts.RemoveAt(parts.Count - 1);
        if (peer.Contains('@') || parts[0] == "github") return "/" + string.Join('/', parts);
        var words = parts.SelectMany(part => part.Split(['-', '_'], StringSplitOptions.RemoveEmptyEntries))
            .Select(word =>
            {
                var lower = word.ToLowerInvariant();
                return Acronyms.Contains(lower) ? lower.ToUpperInvariant() : char.ToUpperInvariant(lower[0]) + lower[1..];
            });
        var name = string.Join(' ', words);
        return name.Length == 0 ? peer : name;
    }

    public static string Input(string value)
    {
        value = value.Trim();
        return value.StartsWith('/') ? value : "/" + value;
    }

    public static string Canonical(string peer) => peer.StartsWith('/') && peer.Count(c => c == '/') == 1
        && !peer.Contains('@') ? peer + "/main" : peer;

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
    // Phase 1b/3a: held out of the inbox by the recipient's filtering; deleted by the server at this time.
    public bool? Filtered { get; init; }
    public long? ExpiresAt { get; init; }
    public string? MsgRef { get; init; }
    public MessageReply? ReplyTo { get; init; }
    public string? EditOf { get; init; }
    public string? EditOfMessageId { get; init; }
    public long? EditedAt { get; init; }
    public int? EditCount { get; init; }
    public MessageReceipt? Receipt { get; init; }
    public MessageAgentStatus? AgentStatus { get; init; }

    [JsonIgnore] public bool IsOutgoing => Direction == "out";
    [JsonIgnore] public long At => IsOutgoing ? SentAt ?? ReceivedAt ?? 0 : ReceivedAt ?? SentAt ?? 0;
    [JsonIgnore] public string PeerKey => PeerHandle ?? Peer ?? (IsOutgoing ? To : SenderHandle ?? From) ?? "unknown";
}

public sealed record MessageAttachment(string Id, string Filename, string MediaType, long Bytes)
{
    [JsonIgnore] public string SizeLabel => Bytes >= 1024 * 1024 ? $"{Bytes / (1024d * 1024):0.#} MB" : Bytes >= 1024 ? $"{Bytes / 1024d:0.#} KB" : $"{Bytes} bytes";
    [JsonIgnore] public string DownloadLabel => $"Download {Filename}";
    [JsonIgnore] public bool CanOpen => AttachmentViewer.Kind(Filename, MediaType) != AttachmentViewKind.None;
    // Browsable files open in the app's viewer; everything else is saved, as before.
    [JsonIgnore] public string OpenLabel => CanOpen ? $"Open {Filename}" : $"Save {Filename}";
    [JsonIgnore] public string ActionText => CanOpen ? "Open" : "Download file";
}
// Local file access stays deferred until Send; never serialize file paths or access callbacks.
public sealed record DraftAttachment(string Id, string Filename, long Bytes, Func<CancellationToken, Task<byte[]>> ReadAsync)
{
    public string RemoveLabel => $"Remove {Filename}";
}

public sealed record Contact(string Peer, string? Alias, string Admission, string Autonomy, IReadOnlyList<string>? AllowedVerbs = null)
{
    [JsonIgnore] public bool IsWildcard => Peer.EndsWith("/*", StringComparison.Ordinal);
}
public sealed record ServerThread(string ThreadId, string Peer, string? Title = null, bool? IsDefault = null,
    long? CreatedAt = null, long? LastAt = null, bool? Archived = null, int? EphemeralSecs = null);
public sealed record SendReceipt(string? MessageId, string? SentCopyId, string? MsgRef = null, bool? Duplicate = null);
public sealed record Vocabulary(IReadOnlyList<string>? Grantable = null, IReadOnlyList<string>? NeverAuto = null)
{
    public IReadOnlyList<string> SafeGrants => (Grantable ?? []).Except(NeverAuto ?? [], StringComparer.Ordinal).Distinct(StringComparer.Ordinal).ToArray();
}

public sealed record InboxSnapshot(IReadOnlyList<InboxMessage> Messages, IReadOnlyList<ServerThread> Threads,
    IReadOnlyList<Contact> Contacts, IReadOnlySet<string> Archived, Vocabulary? Vocabulary = null)
{
    public static InboxSnapshot Empty { get; } = new([], [], [], new HashSet<string>(StringComparer.Ordinal));
}

public enum DeliveryStatus { Sent, Sending, Failed }

public sealed record PendingMessage(string Id, string Mailbox, string To, string Body, long At,
    string? ThreadId, DeliveryStatus Status = DeliveryStatus.Sending, string? SentCopyId = null, IReadOnlyList<MessageAttachment>? Attachments = null);

public sealed record ThreadMessage(string Id, string Body, long At, string? ThreadId, bool IsOutgoing,
    bool Read = true, string? Autonomy = null, string? Verb = null, string? HeldBecause = null,
    DeliveryStatus Status = DeliveryStatus.Sent, IReadOnlyList<MessageAttachment>? Attachments = null,
    bool System = false, long? ExpiresAt = null, string? MsgRef = null, MessageReply? ReplyTo = null,
    long? EditedAt = null, int? EditCount = null, MessageReceipt? Receipt = null, MessageAgentStatus? AgentStatus = null)
{
    // A small centred line in the conversation (a missed call), not a message anyone wrote.
    public bool IsSystem => System;
    public bool Disappears => ExpiresAt is not null;
    public bool CanDownloadAttachments => Status == DeliveryStatus.Sent;
    public bool IsHeld => !IsOutgoing && Autonomy == "review" && !string.IsNullOrEmpty(Verb);
    public string DisplayBody => RequestEnvelope.DisplayText(Body);
    public string CopyText => string.IsNullOrWhiteSpace(DisplayBody) ? AttachmentSummary : DisplayBody;
    public string AttachmentSummary => string.Join(" · ", (Attachments ?? []).Select(a => a.Filename));
    public string EditedLabel => EditCount > 0 && EditedAt is > 0 ? "Edited" : "";
    public string ReceiptLabel => !IsOutgoing || IsSystem || Status != DeliveryStatus.Sent ? "" : Receipt?.State switch
    {
        "delivered" or "seen" when AgentStatus?.State == "working" => "Working",
        "delivered" or "seen" when AgentStatus?.State == "done" => "Done",
        "delivered" or "seen" when AgentStatus?.State == "failed" => "Failed",
        "seen" => "Seen",
        "delivered" => "Delivered",
        "sent" => "Sent",
        _ => ""
    };
    public bool CanEdit(ServerFeatures features, long now) => features.Has(ServerFeatures.MessageEdit) && IsOutgoing
        && !IsSystem && Status == DeliveryStatus.Sent && At > 0 && now >= At && now - At <= 86400
        && (ExpiresAt is null || ExpiresAt > now);
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
    public long Last => Messages.LastOrDefault(m => !m.IsSystem)?.At ?? Messages.LastOrDefault()?.At ?? 0;
    public string Initials => ConversationPresentation.Initials(Name);
    public string ActivityLabel => ConversationPresentation.ActivityLabel(Last, DateTimeOffset.Now);
    public string UnreadLabel => Unread > 99 ? "99+" : Unread.ToString();
    public string ReviewLabel => Held == 1 ? "1 needs review" : $"{Held} need review";
    public bool HasUnread => Unread > 0;
    public bool HasHeld => Held > 0;
    public string AccessibleSummary => $"{Name}, {Peer}. {Preview}. {Badge}";
    public string Preview => Messages.LastOrDefault(m => !m.IsSystem) is { } m ? (string.IsNullOrWhiteSpace(m.DisplayBody) && m.Attachments?.Count > 0 ? m.AttachmentSummary : m.DisplayBody.ReplaceLineEndings(" ")) : "Start a conversation";
    public string Badge => string.Join(" · ", new[] { Unread > 0 ? $"{Unread} unread" : "", Held > 0 ? $"{Held} held" : "" }.Where(s => s.Length > 0));
}

public sealed record Subject(string Id, string? Title, bool IsDefault, IReadOnlyList<ThreadMessage> Messages, long Last, int? EphemeralSecs = null)
{
    public string TimerLabel => Timers.Label(EphemeralSecs);
    public string Name => !string.IsNullOrWhiteSpace(Title) ? Title : IsDefault ? "General" : "Untitled";
    public int Unread => Messages.Count(m => !m.IsOutgoing && !m.Read);
    public bool HasUnread => Unread > 0;
    public string UnreadLabel => Unread > 99 ? "99+" : Unread.ToString();
    public string AccessibleSummary => $"{Name}, {Detail}";
    public string PickerLabel => HasUnread ? $"{Name} · {Unread} unread" : Name;
    public string Detail => $"{Messages.Count} message{(Messages.Count == 1 ? "" : "s")}" + (Unread > 0 ? $" · {Unread} unread" : "");
}

public static class RequestEnvelope
{
    // A file is already message content. Do not invent a caption or an empty work request.
    public static string Attachment(string? caption) => string.IsNullOrWhiteSpace(caption) ? "" : Work(caption.Trim());

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
