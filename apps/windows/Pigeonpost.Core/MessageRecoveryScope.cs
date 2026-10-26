namespace Pigeonpost.Core;

public sealed class MessageRecoveryNotAuthorizedException() : InvalidOperationException("This inbox is not available for delivery recovery.");

// A trusted-exchange owner hint alone never opens stored work. Every scope first authorizes its
// exact canonical inbox against the live server, then retains that account and every caller grant.
public sealed class MessageRecoveryScope : IDisposable
{
    private readonly IInboxService service;
    private readonly AuthorizationLease accepted;
    private readonly AuthorizationLease actor;
    private readonly Action validate;
    private readonly Action<Action> write;
    private bool authorized;
    public string OwnerKey { get; }
    public string Mailbox { get; }

    private MessageRecoveryScope(IInboxService service, AuthorizationLease accepted, AuthorizationLease actor,
        string ownerKey, string mailbox, Action validate, Action<Action> write)
    {
        this.service = service; this.accepted = accepted; this.actor = actor;
        OwnerKey = ownerKey; Mailbox = mailbox; this.validate = validate; this.write = write;
    }

    public static async Task<MessageRecoveryScope?> AuthorizeAsync(IInboxService service, string canonicalMailbox, CancellationToken cancellationToken = default)
    {
        if (!CanonicalMailbox(canonicalMailbox)) throw new ArgumentException("Use a canonical inbox address.", nameof(canonicalMailbox));
        var accepted = service.AcceptedAuthorization;
        var initial = AuthorizationContext.CaptureValidation(() => service.AcceptedAuthorization);
        initial(); cancellationToken.ThrowIfCancellationRequested();
        var ownerKey = service.RecoveryOwnerKey;
        if (accepted is null || !OwnerKeyShape(ownerKey)) return null;
        var actor = accepted.Child();
        try
        {
            using var context = AuthorizationContext.BindActor(accepted, actor);
            var scope = new MessageRecoveryScope(service, accepted, actor, ownerKey!, canonicalMailbox,
                AuthorizationContext.CaptureValidation(() => service.AcceptedAuthorization),
                AuthorizationContext.CaptureWriteBarrier(() => service.AcceptedAuthorization));
            scope.CheckBinding(cancellationToken);
            var mailboxes = await service.GetMailboxesAsync(cancellationToken);
            scope.CheckBinding(cancellationToken);
            if (!mailboxes.Any(m => m.Address == canonicalMailbox)) throw new MessageRecoveryNotAuthorizedException();
            scope.authorized = true;
            return scope;
        }
        catch { actor.Retire(); throw; }
    }

    public bool IsCurrent
    {
        get { try { RequireCurrent(); return true; } catch (AuthorizationRetiredException) { return false; } }
    }
    public void RequireCurrent(CancellationToken cancellationToken = default)
    {
        if (!authorized) throw new AuthorizationRetiredException();
        CheckBinding(cancellationToken);
    }
    private void CheckBinding(CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested(); validate(); actor.RequireActive();
        if (!ReferenceEquals(accepted, service.AcceptedAuthorization) || service.RecoveryOwnerKey != OwnerKey)
            throw new AuthorizationRetiredException();
        AuthorizationContext.CaptureValidation(() => service.AcceptedAuthorization)();
    }
    internal T Local<T>(Func<T> work)
    {
        RequireCurrent();
        T result = default!;
        write(() => result = work());
        RequireCurrent();
        return result;
    }
    private static bool CanonicalMailbox(string address) => address.Length == 29 && address.StartsWith("/k/", StringComparison.Ordinal)
        && address.AsSpan(3).IndexOfAnyExcept("abcdefghijklmnopqrstuvwxyz0123456789") < 0;
    private static bool OwnerKeyShape(string? key) => key is { Length: 64 }
        && key.AsSpan().IndexOfAnyExcept("0123456789abcdef") < 0;
    public void Dispose() => actor.Retire();
    public override string ToString() => "MessageRecoveryScope(redacted)";
}
