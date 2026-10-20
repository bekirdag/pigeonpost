namespace Pigeonpost.Core;

// How much of a new message a Windows toast may show. Windows separately hides toast content on
// the lock screen when the user asks it to; this is the app's own choice for the desktop.
public enum NotificationPreview { NameAndMessage, NameOnly, Hidden }

// What one new-message toast says and how Windows files it. Group keeps one conversation's toasts
// together (and lets "Mark as read" clear them all); Tag lets the app replace or remove one.
public sealed record MessageAlert(string Title, string Text, string Sender, string Initials, string Group, string Tag);

public static class MessageAlerts
{
    public const int MaxPreviewLength = 200;
    public const string GenericTitle = "New Pigeonpost message";
    public const string GenericText = "Open Pigeonpost to read your message.";

    public static MessageAlert Compose(MessageArrival arrival, InboxSnapshot snapshot, IReadOnlyList<Mailbox> mailboxes, NotificationPreview preview)
    {
        var peer = ConversationBuilder.ResolvePeer(arrival.Peer, snapshot, mailboxes);
        var acting = mailboxes.FirstOrDefault(m => m.Address == arrival.Mailbox) ?? new Mailbox(arrival.Mailbox);
        // The same name the conversation list shows: a contact alias, an own mailbox's name, or the
        // name derived from the handle.
        var conversation = ConversationBuilder.Build(snapshot, [], mailboxes, acting).FirstOrDefault(c => c.Peer == peer);
        var sender = conversation?.Name
            ?? (ConversationBuilder.FindContact(peer, snapshot.Contacts)?.Alias is { Length: > 0 } alias ? alias : PostAddress.DisplayName(peer));
        var message = snapshot.Messages.FirstOrDefault(m => m.MessageId == arrival.MessageId);
        var group = GroupKey(arrival.Mailbox, peer);
        var tag = Key(arrival.MessageId);
        var initials = ConversationPresentation.Initials(sender);
        return preview switch
        {
            NotificationPreview.Hidden => new(GenericTitle, GenericText, sender, initials, group, tag),
            NotificationPreview.NameOnly => new(sender, "New message", sender, initials, group, tag),
            _ => new(sender, PreviewText(message), sender, initials, group, tag),
        };
    }

    public static string PreviewText(InboxMessage? message)
    {
        if (message is null) return "New message";
        var text = RequestEnvelope.DisplayText(message.Body).ReplaceLineEndings(" ").Trim();
        if (text.Length == 0 && message.Attachments is { Count: > 0 } files)
            text = files.Count == 1 ? "Sent a file: " + files[0].Filename : $"Sent {files.Count} files";
        if (text.Length == 0) return "New message";
        return text.Length <= MaxPreviewLength ? text : text[..(MaxPreviewLength - 1)].TrimEnd() + "…";
    }

    public static string GroupKey(string mailbox, string peer) => Key(mailbox + "\n" + peer);

    public static string Key(string value) => Convert.ToHexString(
        System.Security.Cryptography.SHA256.HashData(System.Text.Encoding.UTF8.GetBytes(value)))[..16];
}
