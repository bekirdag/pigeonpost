namespace Pigeonpost.Core;

// A control retains the reviewed row's authorization and selection when it is loaded. It may
// not recapture a new account or selection when a queued click eventually runs.
public sealed class MessageTarget
{
    private int active = 1;
    private readonly Func<bool> current;
    private readonly Func<bool> accountCurrent;
    private readonly Func<AuthorizationContext> bind;
    internal readonly InboxViewModel Owner;
    public ThreadMessage Message { get; }
    internal MessageTarget(InboxViewModel owner, ThreadMessage message, Func<bool> current, Func<bool> accountCurrent, Func<AuthorizationContext> bind)
    { Owner = owner; Message = message; this.current = current; this.accountCurrent = accountCurrent; this.bind = bind; }
    public bool IsCurrent => Volatile.Read(ref active) != 0 && current();
    public bool IsAccountCurrent => accountCurrent();
    public void Retire() => Interlocked.Exchange(ref active, 0);
    internal AuthorizationContext BindForAccount()
    {
        if (Volatile.Read(ref active) == 0 || !IsAccountCurrent) throw new AuthorizationRetiredException();
        var context = bind();
        try { if (Volatile.Read(ref active) == 0 || !IsAccountCurrent) throw new AuthorizationRetiredException(); return context; }
        catch { context.Dispose(); throw; }
    }
    public override string ToString() => "MessageTarget(redacted)";
}

// A rendered row belongs to its accepted account, not to the short-lived refresh that painted it.
// Each explicit UI action still creates a separate MessageTarget retaining all action callers.
public sealed class MessageViewBinding
{
    private int active = 1;
    private readonly Func<bool> current, accountCurrent;
    private readonly Func<MessageTarget?> begin;
    public ThreadMessage Message { get; }
    internal MessageViewBinding(ThreadMessage message, Func<bool> current, Func<bool> accountCurrent, Func<MessageTarget?> begin)
    { Message = message; this.current = current; this.accountCurrent = accountCurrent; this.begin = begin; }
    public bool IsCurrent => Volatile.Read(ref active) != 0 && current();
    public bool IsAccountCurrent => accountCurrent();
    public MessageTarget? BeginAction() => IsCurrent ? begin() : null;
    public void Retire() => Interlocked.Exchange(ref active, 0);
    public override string ToString() => "MessageViewBinding(redacted)";
}

public sealed partial class InboxViewModel
{
    public MessageViewBinding? CaptureMessageView(ThreadMessage message)
    {
        using var admission = BeginOperation();
        if (admission is null || account is not { } owner || message.IsSystem || message.Status != DeliveryStatus.Sent
            || !Messages.Any(m => m.Id == message.Id && StableList.SameMessage(m, message))) return null;
        var generation = mailboxVersion;
        var context = ContextVersion;
        bool Current() => AccountCurrent(owner) && generation == mailboxVersion && context == ContextVersion
            && !Timers.Expired(message.ExpiresAt, MessageNow)
            && Messages.Any(m => m.Id == message.Id && StableList.SameMessage(m, message));
        return new(message, Current, () => AccountCurrent(owner), () => Current() ? CaptureMessageTarget(message) : null);
    }

    public MessageTarget? CaptureMessageTarget(ThreadMessage message)
    {
        using var operation = BeginOperation();
        if (operation is null || message.IsSystem || message.Status != DeliveryStatus.Sent
            || !Messages.Any(m => m.Id == message.Id && StableList.SameMessage(m, message))) return null;
        var generation = mailboxVersion;
        var context = ContextVersion;
        return new MessageTarget(this, message, () => operation.IsMailbox(generation) && context == ContextVersion
            && !Timers.Expired(message.ExpiresAt, MessageNow)
            && Messages.Any(m => m.Id == message.Id && StableList.SameMessage(m, message)), () => operation.IsCurrent, AuthorizationContext.CaptureBinding());
    }

    public ThreadMessage? OpenReplyParent(MessageTarget target)
    {
        if (!ReferenceEquals(target.Owner, this) || !target.IsCurrent || target.Message.ReplyTo is not { } reply
            || SelectedConversation is not { } conversation) return null;
        var parent = conversation.Messages.FirstOrDefault(m => !m.IsSystem && m.Status == DeliveryStatus.Sent
            && !Timers.Expired(m.ExpiresAt, MessageNow)
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
