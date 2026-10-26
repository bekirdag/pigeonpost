namespace Pigeonpost.Core;

public sealed class AuthorizationRetiredException() : OperationCanceledException("This sign-in context is no longer active.");

/// <summary>A revocable local-work grant, separate from server membership or message autonomy.</summary>
public sealed class AuthorizationLease(AuthorizationLease? parent = null)
{
    private readonly object monitor = new();
    private int active = 1;
    public Guid Id { get; } = Guid.NewGuid();
    public bool IsActive => Volatile.Read(ref active) != 0 && (parent?.IsActive ?? true);
    private AuthorizationLease Root => parent?.Root ?? this;
    public bool SharesAuthorization(AuthorizationLease other) => ReferenceEquals(Root, other.Root);
    public void RequireActive() { if (!IsActive) throw new AuthorizationRetiredException(); }
    public AuthorizationLease Child() => WithActive(() => new AuthorizationLease(this));

    /// <summary>Never await or acquire session locks inside this short synchronous local write.</summary>
    public T WithActive<T>(Func<T> work)
    {
        RequireActive();
        T Admitted()
        {
            lock (monitor) { RequireActive(); return work(); }
        }
        return parent is null ? Admitted() : parent.WithActive(Admitted);
    }
    public void Retire()
    {
        Interlocked.Exchange(ref active, 0);
        // Stop new work immediately, then finish already-admitted local writes before replacement.
        lock (monitor) { }
    }
    public override string ToString() => "AuthorizationLease(redacted)";
}

/// <summary>Carry the immutable actor across all awaits of a multi-request operation.</summary>
public sealed class AuthorizationContext : IDisposable
{
    private sealed record Frame(AuthorizationLease? Lease);
    private static readonly AsyncLocal<Frame?> Slot = new();
    private readonly Frame? previous;
    private readonly Frame frame;
    public static AuthorizationLease? Current => Slot.Value?.Lease;
    internal static bool IsBound => Slot.Value is not null;
    public AuthorizationContext(AuthorizationLease lease) : this(lease ?? throw new ArgumentNullException(nameof(lease)), true) { }
    private AuthorizationContext(AuthorizationLease? lease, bool _)
    {
        lease?.RequireActive(); previous = Slot.Value; frame = new(lease); Slot.Value = frame;
    }
    internal static AuthorizationContext Capture(AuthorizationLease? fallback) => new(IsBound ? Current : fallback, true);
    public void Dispose() { if (ReferenceEquals(Slot.Value, frame)) Slot.Value = previous; }
}
