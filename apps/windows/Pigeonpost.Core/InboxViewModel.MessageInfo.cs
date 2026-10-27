namespace Pigeonpost.Core;

public sealed partial class InboxViewModel
{
    private ServerFeatures messageFeatures = ServerFeatures.None;
    public bool CanInspectMessages => ActiveAccount is not null && service is IMessageActionsService
        && new[] { ServerFeatures.MessageEdit, ServerFeatures.MessageReply, ServerFeatures.Receipts, ServerFeatures.AgentStatus }.Any(messageFeatures.Has);

    public void SetMessageFeatures(ServerFeatures features)
    {
        using var operation = BeginOperation();
        if (operation is null) return;
        var copy = new ServerFeatures(new HashSet<string>(features.Names, StringComparer.Ordinal));
        operation.Local(() => messageFeatures = copy);
        if (operation.IsCurrent) Changed(nameof(CanInspectMessages));
    }

    public async Task<MessageInfo?> ReadMessageInfoAsync(MessageTarget target, CancellationToken cancellationToken = default)
    {
        if (!ReferenceEquals(target.Owner, this) || !target.IsCurrent || !CanInspectMessages
            || service is not IMessageActionsService actions || SelectedMailbox is not { } mailbox) return null;
        using var binding = target.BindForAccount();
        using var operation = BeginOperation();
        if (operation is null) return null;
        try
        {
            operation.RequireService(actions.AcceptedAuthorization);
            var info = await actions.GetMessageInfoAsync(mailbox.Address, target.Message.Id, cancellationToken);
            operation.Check(cancellationToken);
            if (!target.IsCurrent || !CanInspectMessages) return null;
            if (info.MessageId != target.Message.Id) throw new PostboxException(502, "bad_response");
            return info;
        }
        catch (OperationCanceledException) { return null; }
        catch (Exception ex)
        {
            if (target.IsCurrent && operation.IsCurrent) Error = Describe(ex);
            return null;
        }
    }
}
