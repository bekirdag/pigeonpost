using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Serialization;

namespace Pigeonpost.Core;

// Server action metadata is data, never a grant to act on a message body.
public sealed record MessageReply(string? MsgRef = null, string? MessageId = null, string? From = null,
    string? Direction = null, string? Excerpt = null, string? Quote = null, string? Verb = null, JsonElement? Args = null)
{
    [JsonIgnore] public string DisplayQuote => RequestEnvelope.DisplayText(Excerpt ?? Quote ?? "Message unavailable");
}
public sealed record MessageReceipt(string? State = null, long? DeliveredAt = null, long? SeenAt = null);
public sealed record MessageAgentStatus(string? State = null, long? At = null);
public sealed record MessageVersion(string MessageId, string Body, long At);
public sealed record MessageInfo(string MessageId, string? MsgRef = null, string? Direction = null, long? SentAt = null,
    IReadOnlyList<MessageVersion>? Versions = null, MessageReply? ReplyTo = null, MessageReceipt? Receipt = null,
    MessageAgentStatus? AgentStatus = null);

public static class MessageReferences
{
    public static string New()
    {
        const string alphabet = "abcdefghijklmnopqrstuvwxyz234567";
        var result = new StringBuilder("mref_", 31);
        uint buffer = 0;
        var bits = 0;
        foreach (var value in RandomNumberGenerator.GetBytes(16))
        {
            buffer = (buffer << 8) | value;
            bits += 8;
            while (bits >= 5)
            {
                bits -= 5;
                result.Append(alphabet[(int)((buffer >> bits) & 31)]);
            }
        }
        if (bits > 0) result.Append(alphabet[(int)((buffer << (5 - bits)) & 31)]);
        return result.ToString();
    }
    public static bool IsValid(string value) => value.Length == 31 && value.StartsWith("mref_", StringComparison.Ordinal)
        && value.AsSpan(5).IndexOfAnyExcept("abcdefghijklmnopqrstuvwxyz234567") < 0;

    public static bool IsReferenceOrCopyId(string value) => value.Length is >= 1 and <= 256
        && value.All(c => char.IsAsciiLetterOrDigit(c) || c is '_' or ':' or '-');
}

// Retain this exact immutable request for an explicit retry after an uncertain response.
// A new instance represents a new delivery intent; never silently replay it with a new ref.
public sealed class MessageSendRequest
{
    public string From { get; }
    public string To { get; }
    public string Body { get; }
    public string? ThreadId { get; }
    public IReadOnlyList<string>? Attachments { get; }
    public string MsgRef { get; }
    public string? ReplyTo { get; }
    public string? Quote { get; }
    public string? EditOf { get; }

    public MessageSendRequest(string from, string to, string body, string? threadId = null,
        IEnumerable<string>? attachments = null, string? msgRef = null, string? replyTo = null, string? quote = null, string? editOf = null)
    {
        if (!PostAddress.IsValid(from) || !PostAddress.IsValid(to)) throw new ArgumentException("Invalid message address.");
        MsgRef = msgRef ?? MessageReferences.New();
        if (!MessageReferences.IsValid(MsgRef)) throw new ArgumentException("Invalid message reference.");
        if (replyTo is not null && editOf is not null) throw new ArgumentException("A message cannot be both an edit and a reply.");
        if (new[] { replyTo, editOf }.Any(r => r is not null && !MessageReferences.IsReferenceOrCopyId(r)))
            throw new ArgumentException("Invalid referenced message.");
        if (quote is not null && (replyTo is null || quote.EnumerateRunes().Count() > 200))
            throw new ArgumentException("A reply quote needs a parent and at most 200 characters.");
        var files = attachments?.ToArray();
        if (files is { Length: > 0 } && (editOf is not null || files.Any(f => !MessageReferences.IsReferenceOrCopyId(f))))
            throw new ArgumentException("Invalid message attachments.");
        From = from; To = to; Body = body; ThreadId = threadId;
        Attachments = files is { Length: > 0 } ? Array.AsReadOnly(files) : null;
        ReplyTo = replyTo; Quote = quote; EditOf = editOf;
    }
    public override string ToString() => "MessageSendRequest(redacted)";
}

public interface IMessageActionsService
{
    AuthorizationLease? AcceptedAuthorization => null;
    Task<SendReceipt> SendMessageAsync(MessageSendRequest request, CancellationToken cancellationToken);
    Task<MessageInfo> GetMessageInfoAsync(string identity, string messageId, CancellationToken cancellationToken);
    Task DeleteMessageAsync(string identity, string messageId, CancellationToken cancellationToken);
}
