namespace Pigeonpost.Core;

public sealed partial class PostboxClient : IMessageActionsService, IProtectedMessageService
{
    AuthorizationLease? IMessageActionsService.AcceptedAuthorization => AcceptedAuthorization;

    public async Task<RecipientRoute> ResolveRecipientAsync(MessageRecoveryScope scope, string reviewedRecipient, CancellationToken cancellationToken)
    {
        using var binding = scope.BindForService(this);
        scope.RequireCurrent(cancellationToken);
        if (!ValidRecipient(reviewedRecipient)) throw new ArgumentException("Use a Pigeonpost handle or inbox address.");
        var features = await GetCapabilitiesAsync(cancellationToken).ConfigureAwait(false);
        scope.RequireCurrent(cancellationToken);
        if (!features.Has(ServerFeatures.RecipientLookup)) throw new PostboxException(403, "feature_disabled");
        var route = await WriteAsync<RecipientRoute>(HttpMethod.Post, "/v1/recipients/resolve",
            new { identity = scope.Mailbox, to = reviewedRecipient }, cancellationToken).ConfigureAwait(false);
        scope.RequireCurrent(cancellationToken);
        if (route.From != scope.Mailbox || route.Address is null || !MessageRecoveryScope.CanonicalMailbox(route.Address)
            || route.Handle is not null && !ValidRecipient(route.Handle))
            throw new PostboxException(502, "bad_response");
        return route;
    }

    private static bool ValidRecipient(string? value) => value is not null && System.Text.Encoding.UTF8.GetByteCount(value) <= 512
        && !value.Any(char.IsControl) && PostAddress.IsValid(value);

    public Task<SendReceipt> SendMessageAsync(MessageSendRequest request, CancellationToken cancellationToken) =>
        WriteAsync<SendReceipt>(HttpMethod.Post, "/v1/send", request, cancellationToken);

    public async Task<SendReceipt> SendPreparedMessageAsync(MessageRecoveryScope scope, PreparedMessageAttempt attempt, CancellationToken cancellationToken)
    {
        attempt.RequireScope(scope);
        using var binding = scope.BindForService(this);
        scope.RequireCurrent(cancellationToken);
        var features = await GetCapabilitiesAsync(cancellationToken).ConfigureAwait(false);
        scope.RequireCurrent(cancellationToken);
        if (!new[] { ServerFeatures.MessageEdit, ServerFeatures.MessageReply, ServerFeatures.Receipts, ServerFeatures.AgentStatus }.Any(features.Has)
            || attempt.Request.EditOf is not null && !features.Has(ServerFeatures.MessageEdit)
            || attempt.Request.ReplyTo is not null && !features.Has(ServerFeatures.MessageReply))
            throw new PostboxException(403, "feature_disabled");
        using var response = await RequestAsync(HttpMethod.Post, "/v1/send", attempt.WireJson, cancellationToken).ConfigureAwait(false);
        var receipt = await DecodeAsync<SendReceipt>(response, cancellationToken).ConfigureAwait(false);
        scope.RequireCurrent(cancellationToken);
        if (receipt.MsgRef != attempt.Request.MsgRef) throw new PostboxException(502, "bad_response");
        return receipt;
    }

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
