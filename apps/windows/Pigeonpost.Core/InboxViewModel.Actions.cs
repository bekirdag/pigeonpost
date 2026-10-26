namespace Pigeonpost.Core;

// A control retains the reviewed row's authorization and selection when it is loaded. It may
// not recapture a new account or selection when a queued click eventually runs.
public sealed class MessageTarget
{
    private int active = 1;
    private readonly Func<bool> current;
    private readonly Func<bool> accountCurrent;
    internal readonly InboxViewModel Owner;
    public ThreadMessage Message { get; }
    internal MessageTarget(InboxViewModel owner, ThreadMessage message, Func<bool> current, Func<bool> accountCurrent)
    { Owner = owner; Message = message; this.current = current; this.accountCurrent = accountCurrent; }
    public bool IsCurrent => Volatile.Read(ref active) != 0 && current();
    public bool IsAccountCurrent => accountCurrent();
    public void Retire() => Interlocked.Exchange(ref active, 0);
    public override string ToString() => "MessageTarget(redacted)";
}

public sealed partial class InboxViewModel
{
    public MessageTarget? CaptureMessageTarget(ThreadMessage message)
    {
        using var operation = BeginOperation();
        if (operation is null || message.IsSystem || message.Status != DeliveryStatus.Sent
            || !Messages.Any(m => m.Id == message.Id && StableList.SameMessage(m, message))) return null;
        var generation = mailboxVersion;
        var context = ContextVersion;
        return new MessageTarget(this, message, () => operation.IsMailbox(generation) && context == ContextVersion
            && !Timers.Expired(message.ExpiresAt, DateTimeOffset.UtcNow.ToUnixTimeSeconds())
            && Messages.Any(m => m.Id == message.Id && StableList.SameMessage(m, message)), () => operation.IsCurrent);
    }

    public ThreadMessage? OpenReplyParent(MessageTarget target)
    {
        if (!ReferenceEquals(target.Owner, this) || !target.IsCurrent || target.Message.ReplyTo is not { } reply
            || SelectedConversation is not { } conversation) return null;
        var parent = conversation.Messages.FirstOrDefault(m => !m.IsSystem && m.Status == DeliveryStatus.Sent
            && !Timers.Expired(m.ExpiresAt, DateTimeOffset.UtcNow.ToUnixTimeSeconds())
            && (reply.MessageId is { Length: > 0 } id ? m.Id == id
                && (reply.MsgRef is not { Length: > 0 } reference || m.MsgRef == reference)
                : reply.MsgRef is { Length: > 0 } referenceOnly && m.MsgRef == referenceOnly));
        if (parent is null)
        {
            if (target.IsCurrent) Error = "The original message is unavailable. Its quote is still shown.";
            return null;
        }
        if (!target.IsCurrent) return null;
        var subject = Subjects.FirstOrDefault(s => s.Messages.Any(m => m.Id == parent.Id));
        if (subject is null) return null;
        SelectSubject(subject);
        return Messages.FirstOrDefault(m => m.Id == parent.Id);
    }
}
