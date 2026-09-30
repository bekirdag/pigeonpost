namespace Pigeonpost.Core;

public interface IInboxService
{
    Task<IReadOnlyList<Mailbox>> GetMailboxesAsync(CancellationToken cancellationToken);
    Task<InboxSnapshot> LoadAsync(string identity, CancellationToken cancellationToken);
    Task<SendReceipt> SendAsync(string identity, string peer, string body, string? threadId, CancellationToken cancellationToken);
    Task<string> CreateThreadAsync(string identity, string peer, string title, CancellationToken cancellationToken);
    Task SetArchivedAsync(string identity, string peer, bool archived, CancellationToken cancellationToken);
    Task AcknowledgeAsync(string identity, string messageId, CancellationToken cancellationToken);
}

public interface IAccessTokenProvider
{
    Task<string> GetTokenAsync(CancellationToken cancellationToken);
    Task<string> RefreshTokenAsync(CancellationToken cancellationToken);
}
