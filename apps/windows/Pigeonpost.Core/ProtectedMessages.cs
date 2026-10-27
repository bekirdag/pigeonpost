namespace Pigeonpost.Core;

public sealed record RecipientRoute(string From, string Address, string? Handle)
{
    public override string ToString() => "RecipientRoute(redacted)";
}

public interface IProtectedMessageService : IInboxService
{
    Task<RecipientRoute> ResolveRecipientAsync(MessageRecoveryScope scope, string reviewedRecipient, CancellationToken cancellationToken);
    Task<SendReceipt> SendPreparedMessageAsync(MessageRecoveryScope scope, PreparedMessageAttempt attempt, CancellationToken cancellationToken);
}
