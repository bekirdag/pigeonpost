namespace Pigeonpost.Core;

public sealed partial class PostboxClient : IMessageActionsService
{
    AuthorizationLease? IMessageActionsService.AcceptedAuthorization => AcceptedAuthorization;

    public Task<SendReceipt> SendMessageAsync(MessageSendRequest request, CancellationToken cancellationToken) =>
        WriteAsync<SendReceipt>(HttpMethod.Post, "/v1/send", request, cancellationToken);

    public Task<MessageInfo> GetMessageInfoAsync(string identity, string messageId, CancellationToken cancellationToken)
    {
        RequireMessageCopyId(messageId);
        return GetJsonAsync<MessageInfo>("/v1/messages/" + Uri.EscapeDataString(messageId) + "/info", identity, cancellationToken);
    }

    public Task DeleteMessageAsync(string identity, string messageId, CancellationToken cancellationToken)
    {
        RequireMessageCopyId(messageId);
        return SendAsync(HttpMethod.Post, "/v1/messages/delete", new { identity, message_id = messageId }, identity, cancellationToken);
    }

    private static void RequireMessageCopyId(string messageId)
    {
        if (!MessageReferences.IsReferenceOrCopyId(messageId)) throw new ArgumentException("Invalid message copy.");
    }
}
