namespace Pigeonpost.Core;

// One accepted login and canonical inbox for one native call host. Recreating a host retires
// its token requests; ordinary token renewal keeps the accepted login's existing grant.
public sealed class NativeCallSession : IDisposable
{
    private readonly IAccessTokenProvider provider;
    private readonly AuthorizationLease? accepted;
    private readonly AuthorizationLease authorization;
    public Mailbox Mailbox { get; }

    public NativeCallSession(IAccessTokenProvider provider, Mailbox mailbox, bool allowUnboundFixture = false)
    {
        this.provider = provider;
        Mailbox = mailbox;
        accepted = provider.AcceptedAuthorization;
        if (accepted is null && !allowUnboundFixture) throw new AuthorizationRetiredException();
        authorization = accepted?.Child() ?? new AuthorizationLease();
    }
    public bool IsActive
    {
        get { try { RequireActive(); return true; } catch (AuthorizationRetiredException) { return false; } }
    }
    public bool Matches(Mailbox mailbox) => IsActive && Mailbox.Address == mailbox.Address;
    public void RequireActive()
    {
        authorization.RequireActive();
        if (!ReferenceEquals(accepted, provider.AcceptedAuthorization)) throw new AuthorizationRetiredException();
        AuthorizationContext.CaptureValidation(provider)();
    }
    public async Task<string> TokenAsync(bool renew, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        RequireActive();
        using var actor = AuthorizationContext.BindActor(accepted, authorization);
        var validate = AuthorizationContext.CaptureValidation(provider);
        var token = await (renew ? provider.RefreshTokenAsync(cancellationToken) : provider.GetTokenAsync(cancellationToken)).ConfigureAwait(false);
        cancellationToken.ThrowIfCancellationRequested();
        validate(); RequireActive();
        return token;
    }
    public void Dispose() => authorization.Retire();
    public override string ToString() => "NativeCallSession(redacted)";
}
