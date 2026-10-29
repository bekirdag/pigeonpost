namespace Pigeonpost.Core;

public sealed partial class InboxViewModel
{
    private IReadOnlyDictionary<string, PeerName> peerNames = new Dictionary<string, PeerName>();
    public bool ApplyPeerNames(NewConversationTarget target, IReadOnlyDictionary<string, PeerName> names)
    {
        if (!ReferenceEquals(target.Owner, this) || !target.IsCurrent) return false;
        using var context = target.Bind();
        // Copy so a stale caller cannot mutate a dictionary after its authorization is retired.
        peerNames = new Dictionary<string, PeerName>(names, StringComparer.Ordinal);
        Rebuild();
        return true;
    }
    public async Task<T> WithPeopleTargetAsync<T>(NewConversationTarget target, PostboxClient client,
        Func<string, CancellationToken, Task<T>> action)
    {
        if (!ReferenceEquals(target.Owner, this) || !target.IsCurrent || !ReferenceEquals(client.AcceptedAuthorization, service.AcceptedAuthorization))
            throw new AuthorizationRetiredException();
        using var context = target.Bind();
        var result = await action(target.Mailbox, mailboxScope.Token);
        if (!target.IsCurrent) throw new AuthorizationRetiredException();
        return result;
    }
}
