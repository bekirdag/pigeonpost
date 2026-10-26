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
    private sealed record Frame(AuthorizationLease? Lease, AuthorizationLease? LocalConstraint, Frame? Parent);
    private static readonly AsyncLocal<Frame?> Slot = new();
    private readonly Frame? previous;
    private readonly Frame frame;
    public static AuthorizationLease? Current => Slot.Value?.Lease;
    internal static bool IsBound => Slot.Value is not null;
    public AuthorizationContext(AuthorizationLease lease) : this(lease ?? throw new ArgumentNullException(nameof(lease)), true) { }
    private AuthorizationContext(AuthorizationLease? lease, bool _) : this(lease, lease) { }
    private AuthorizationContext(AuthorizationLease? lease, AuthorizationLease? localConstraint)
    {
        lease?.RequireActive(); localConstraint?.RequireActive();
        previous = Slot.Value; frame = new(lease, localConstraint, previous); Slot.Value = frame;
    }
    private AuthorizationContext(Frame captured)
    {
        previous = Slot.Value;
        var frames = new List<Frame>();
        for (var scope = captured; scope is not null; scope = scope.Parent) frames.Add(scope);
        Frame? merged = previous;
        for (var index = frames.Count - 1; index >= 0; index--)
        {
            var scope = frames[index];
            scope.Lease?.RequireActive(); scope.LocalConstraint?.RequireActive();
            merged = new(scope.Lease, scope.LocalConstraint, merged);
        }
        frame = merged!; Slot.Value = frame;
    }
    internal static AuthorizationContext Capture(AuthorizationLease? fallback) => new(IsBound ? Current : fallback, true);
    internal static AuthorizationContext BindActor(AuthorizationLease? accepted, AuthorizationLease actor)
    {
        actor.RequireActive();
        if (accepted is not null && !actor.SharesAuthorization(accepted)) throw new AuthorizationRetiredException();
        // A fixed-credential stand-in may lack a login grant. Its local actor still cancels work;
        // it never becomes network authority or replaces a real AccountSession's grant.
        return new(accepted is null ? null : actor, actor);
    }
    internal static Func<AuthorizationContext> CaptureBinding()
    {
        var captured = Slot.Value ?? throw new AuthorizationRetiredException();
        return () => new AuthorizationContext(captured);
    }
    internal static Action CaptureValidation(IAccessTokenProvider provider) => CaptureValidation(() => provider.AcceptedAuthorization);
    internal static Action CaptureValidation(Func<AuthorizationLease?> currentAuthorization)
    {
        var accepted = currentAuthorization();
        var captured = Slot.Value ?? new Frame(accepted, accepted, null);
        return () =>
        {
            var current = currentAuthorization();
            if (!ReferenceEquals(current, accepted)) throw new AuthorizationRetiredException();
            for (var scope = captured; scope is not null; scope = scope.Parent)
            {
                scope.LocalConstraint?.RequireActive();
                if (scope.Lease is null ? current is not null : current is null || !scope.Lease.SharesAuthorization(current))
                    throw new AuthorizationRetiredException();
            }
        };
    }
    // Admit only short synchronous private writes; never run UI callbacks, awaits or session
    // getters inside this barrier. All captured grants share one root, which orders the locks.
    internal static Action<Action> CaptureWriteBarrier(Func<AuthorizationLease?> currentAuthorization)
    {
        var accepted = currentAuthorization() ?? throw new AuthorizationRetiredException();
        var validate = CaptureValidation(currentAuthorization);
        validate();
        var grants = new List<AuthorizationLease>();
        for (var scope = Slot.Value; scope is not null; scope = scope.Parent)
        {
            foreach (var grant in new[] { scope.Lease, scope.LocalConstraint })
                if (grant is not null && !grants.Contains(grant))
                {
                    if (!grant.SharesAuthorization(accepted)) throw new AuthorizationRetiredException();
                    grants.Add(grant);
                }
        }
        if (!grants.Contains(accepted)) grants.Add(accepted);
        return work =>
        {
            validate(); // The provider may acquire a session lock; keep this outside every grant.
            void Admit(int index)
            {
                if (index == grants.Count) { work(); return; }
                grants[index].WithActive(() => { Admit(index + 1); return 0; });
            }
            Admit(0);
        };
    }
    public void Dispose() { if (ReferenceEquals(Slot.Value, frame)) Slot.Value = previous; }
}
