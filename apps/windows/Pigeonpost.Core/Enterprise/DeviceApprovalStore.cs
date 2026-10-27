namespace Pigeonpost.Core.Enterprise;

// The signed-in human and the exact accepted grant. The owner key is a hashed principal hint from
// the trusted exchange; it never authorizes anything by itself.
public sealed class DeviceApprovalIdentity(string ownerKey, AuthorizationLease authorization)
{
    public string OwnerKey { get; } = ownerKey;
    public AuthorizationLease Authorization { get; } = authorization;
    public bool Same(DeviceApprovalIdentity? other) => other is not null && OwnerKey == other.OwnerKey
        && ReferenceEquals(Authorization, other.Authorization);
    public override string ToString() => "DeviceApprovalIdentity(redacted)";
}

public enum DeviceApprovalPhase { Idle, Resolving, Review, Approving, Denying, Verifying, Approved, Denied, Expired, Unconfirmed, Unavailable }

public sealed record DeviceApprovalState
{
    public string ContextId { get; init; } = NewId();
    // Replaced whenever the reviewed screen changes: a decision must name the screen it came from.
    public string ProofId { get; init; } = NewId();
    // Terminal proof, kept in this view's memory only.
    internal DevicePairingCode? Code { get; init; }
    public bool HasCode => Code is not null;
    public DevicePairingPreview? Preview { get; init; }
    public DeviceApprovalPhase Phase { get; init; } = DeviceApprovalPhase.Idle;
    public StrongSignInRequirement? Challenge { get; init; }
    public string? Message { get; init; }
    public long? RetryAt { get; init; }
    public bool Busy => Phase is DeviceApprovalPhase.Resolving or DeviceApprovalPhase.Approving
        or DeviceApprovalPhase.Denying or DeviceApprovalPhase.Verifying;
    public bool NeedsVerification => Phase == DeviceApprovalPhase.Review && Challenge is not null;
    internal static string NewId() => Guid.NewGuid().ToString("N");
    public override string ToString() => $"DeviceApprovalState({Phase})";
}

// Human account scope, independent of the selected inbox. Construct and use on the UI context.
// Nothing is decided on arrival, after verification or after a lost reply: every approval or denial
// is one explicit human action on the currently reviewed screen, and is never resent automatically.
public sealed class DeviceApprovalStore(IDevicePairingApi api, Func<DeviceApprovalIdentity?> identity,
    Func<bool>? transitioning = null, TimeProvider? clock = null) : ObservableObject, IDisposable
{
    private readonly Func<bool> transitioning = transitioning ?? (() => false);
    private readonly TimeProvider clock = clock ?? TimeProvider.System;
    private DeviceApprovalState state = new();
    private DeviceApprovalIdentity? bound;
    private AuthorizationLease? actor;
    private AuthorizationLease? request;
    private CancellationTokenSource? work;
    private Guid operation = Guid.NewGuid();
    private string? rateOwner;
    private long? deadline;
    private bool disposed;

    public DeviceApprovalState State => state;
    private long Now => clock.GetUtcNow().ToUnixTimeSeconds();
    private void Publish(DeviceApprovalState next) { state = next; Changed(nameof(State)); }

    private DeviceApprovalIdentity? Live()
    {
        if (disposed || identity() is not { } next || string.IsNullOrEmpty(next.OwnerKey)
            || System.Text.Encoding.UTF8.GetByteCount(next.OwnerKey) > 256 || !next.Authorization.IsActive
            || !ReferenceEquals(api.AcceptedAuthorization, next.Authorization)) return null;
        return next;
    }
    public bool IsCurrent(string? context = null) => actor?.IsActive == true
        && (context ?? state.ContextId) == state.ContextId && bound?.Same(Live()) == true;
    private bool Current(AuthorizationLease owner, Guid visit) => ReferenceEquals(actor, owner) && operation == visit && IsCurrent();

    private void Stop()
    {
        operation = Guid.NewGuid();
        request?.Retire(); request = null;
        work?.Cancel(); work = null;
    }
    private void Bind(DeviceApprovalIdentity? next, bool preserveRate = false)
    {
        Stop(); actor?.Retire(); actor = null; bound = null;
        if (!preserveRate && next?.OwnerKey != rateOwner) { rateOwner = null; deadline = null; }
        AuthorizationLease? child = null;
        try { child = next?.Authorization.Child(); } catch (AuthorizationRetiredException) { }
        if (child is not null) { actor = child; bound = next; }
        Publish(new DeviceApprovalState { RetryAt = deadline });
    }

    public void Synchronize()
    {
        var next = Live();
        if (bound?.Same(next) == true && actor?.IsActive == true) return;
        // Only the awaited verification may bridge the old grant's retirement and adopt the new one.
        if (state.Phase == DeviceApprovalPhase.Verifying && bound is not null
            && (next is null && transitioning() || next is not null && next.OwnerKey == bound.OwnerKey)) return;
        if (next is null && bound is null && actor is null) return;
        Bind(next);
    }

    public void Close() => Bind(null, preserveRate: Live()?.OwnerKey == rateOwner);
    public void Dispose() { if (disposed) return; Bind(null); disposed = true; }

    public void ChangeCode(string context)
    {
        if (!IsCurrent(context) || state.Busy) return;
        Stop();
        Publish(new DeviceApprovalState { ContextId = state.ContextId, RetryAt = deadline });
    }

    public void RefreshExpiry()
    {
        if (deadline is { } until && until <= Now) { deadline = null; Publish(state with { RetryAt = null }); }
        if (state.Phase == DeviceApprovalPhase.Review && state.Preview is { } preview && preview.ExpiresAt <= Now)
            Publish(state with { ProofId = DeviceApprovalState.NewId(), Phase = DeviceApprovalPhase.Expired, Challenge = null,
                Message = "This request expired. Start again on the machine." });
    }

    public bool CanApprove(string proof, string context) => IsCurrent(context) && state.ProofId == proof
        && state.Phase == DeviceApprovalPhase.Review && state.Challenge is null && state.Code is not null
        && state.Preview?.AllowsApproval(Now) == true;
    public bool CanDeny(string proof, string context) => IsCurrent(context) && state.ProofId == proof
        && state.Phase == DeviceApprovalPhase.Review && state.Code is not null
        && state.Preview is { CanApprove: true } preview && preview.AllowsDecision(Now);

    private bool Limited()
    {
        if (deadline is not { } until || until <= Now) return false;
        Publish(state with { Message = "Too many attempts. Wait before checking another request.", RetryAt = until });
        return true;
    }

    private Task? Run(DeviceApprovalPhase phase, string context, bool uncertain, Func<AuthorizationLease, Guid, CancellationToken, Task> action)
    {
        Synchronize();
        if (actor is not { } owner || !IsCurrent(context) || state.Busy || Limited()) return null;
        AuthorizationLease child;
        try { child = owner.Child(); } catch (AuthorizationRetiredException) { Synchronize(); return null; }
        Stop();
        var visit = Guid.NewGuid(); var cancellation = new CancellationTokenSource();
        operation = visit; request = child; work = cancellation;
        Publish(state with { Phase = phase, Message = null, ProofId = DeviceApprovalState.NewId() });
        return Execute();
        async Task Execute()
        {
            try
            {
                using (new AuthorizationContext(child)) await action(owner, visit, cancellation.Token);
            }
            catch (Exception failure) { if (Current(owner, visit)) Failed(failure, uncertain); }
            finally
            {
                child.Retire();
                if (Current(owner, visit) && state.Phase == phase)
                    Publish(state with { Phase = uncertain ? DeviceApprovalPhase.Unconfirmed : DeviceApprovalPhase.Idle });
                if (ReferenceEquals(work, cancellation)) { work = null; request = null; }
                cancellation.Dispose();
            }
        }
    }

    public Task? Resolve(string input, string context)
    {
        Synchronize();
        if (!IsCurrent(context) || state.Busy) return null;
        var code = DevicePairingCode.Parse(input) ?? DevicePairingLink.Parse(input);
        if (code is null) { Publish(state with { Message = "Enter the eight-character code shown on the machine." }); return null; }
        return ResolveCode(code, context);
    }
    public Task? CheckStatus(string context) => state.Code is { } code ? ResolveCode(code, context) : null;
    private Task? ResolveCode(DevicePairingCode code, string context) => Run(DeviceApprovalPhase.Resolving, context, false, async (owner, visit, ct) =>
    {
        Publish(state with { Code = code, Preview = null, Challenge = null });
        var loaded = await api.ResolveDevicePairingAsync(code, ct);
        if (Current(owner, visit)) Apply(loaded);
    });

    private void Apply(DevicePairingPreview loaded)
    {
        if (!loaded.IsValid) throw new PostboxException(200, "bad_response");
        var phase = loaded.State switch
        {
            "approved" => DeviceApprovalPhase.Approved,
            "denied" => DeviceApprovalPhase.Denied,
            _ => loaded.ExpiresAt <= Now ? DeviceApprovalPhase.Expired : DeviceApprovalPhase.Review
        };
        var message = phase switch
        {
            DeviceApprovalPhase.Expired => "This request expired. Start again on the machine.",
            DeviceApprovalPhase.Approved => "This machine is approved. It is signing in now.",
            DeviceApprovalPhase.Denied => "This request was denied. The machine cannot use it.",
            _ when !loaded.CanApprove => loaded.Reason == "delegation_required"
                ? "Ask an agent admin to add you as one of its operators." : "Your account cannot decide this request.",
            _ when loaded.Capabilities.Count != 1 || loaded.Capabilities[0] != "mailbox"
                => "This request asks for access this app does not support. Ask your administrator.",
            _ => null
        };
        Publish(state with { Preview = loaded, Phase = phase, ProofId = DeviceApprovalState.NewId(), Challenge = null, Message = message });
    }

    public Task? Approve(string proof, string context)
    {
        RefreshExpiry();
        if (!CanApprove(proof, context)) return null;
        var preview = state.Preview!; var code = state.Code!;
        return Run(DeviceApprovalPhase.Approving, context, true, async (owner, visit, ct) =>
        {
            await api.ApproveDevicePairingAsync(preview, code, ct);
            if (Current(owner, visit))
                Publish(state with { Phase = DeviceApprovalPhase.Approved, Challenge = null, Message = "Approved. You can close this; the machine is signing in." });
        });
    }

    public Task? Deny(string proof, string context)
    {
        RefreshExpiry();
        if (!CanDeny(proof, context)) return null;
        var preview = state.Preview!; var code = state.Code!;
        return Run(DeviceApprovalPhase.Denying, context, true, async (owner, visit, ct) =>
        {
            await api.DenyDevicePairingAsync(preview, code, ct);
            // The acknowledgment alone does not prove that a concurrent approval did not win.
            var loaded = await api.ResolveDevicePairingAsync(code, ct);
            if (!Current(owner, visit)) return;
            if (loaded.State is "approved" or "denied") Apply(loaded);
            else Publish(state with { Phase = DeviceApprovalPhase.Unconfirmed, ProofId = DeviceApprovalState.NewId(),
                Message = "The denial was sent, but its final status is not confirmed. Check it again." });
        });
    }

    // The verified sign-in replaces the accepted grant. Afterwards the same code is read again and
    // another explicit human decision is required; verification never approves by itself.
    public Task? VerifySignIn(string proof, string context,
        Func<StrongSignInRequirement, AuthorizationLease, CancellationToken, Task<AuthorizationLease>> verify)
    {
        RefreshExpiry();
        if (!IsCurrent(context) || state.Phase != DeviceApprovalPhase.Review || state.ProofId != proof || Limited()) return null;
        if (state.Challenge is not { } requirement || bound is not { } original || state.Code is not { } code) return null;
        Stop();
        var visit = Guid.NewGuid(); var cancellation = new CancellationTokenSource();
        operation = visit; work = cancellation;
        Publish(state with { Phase = DeviceApprovalPhase.Verifying, ProofId = DeviceApprovalState.NewId(), Message = null });
        return Execute();
        async Task Execute()
        {
            try
            {
                // Deliberately outside the old actor's context: adoption retires that grant.
                var accepted = await verify(requirement, original.Authorization, cancellation.Token);
                if (operation != visit) return;
                var next = Live();
                work = null;
                if (next is null || next.OwnerKey != original.OwnerKey || !ReferenceEquals(next.Authorization, accepted)
                    || ReferenceEquals(accepted, original.Authorization) || !accepted.IsActive)
                {
                    Bind(next);
                    Publish(state with { Message = "Your sign-in changed. Enter the machine's code again." });
                    return;
                }
                Bind(next, preserveRate: true);
                if (ResolveCode(code, state.ContextId) is { } reread) await reread;
            }
            catch (OperationCanceledException) when (operation == visit && cancellation.IsCancellationRequested)
            {
                work = null;
                Publish(state with { Phase = DeviceApprovalPhase.Review, ProofId = DeviceApprovalState.NewId(), Message = "Verification was cancelled." });
                Synchronize();
            }
            catch (Exception) when (operation == visit)
            {
                work = null;
                Publish(state with { Phase = DeviceApprovalPhase.Review, ProofId = DeviceApprovalState.NewId(),
                    Message = "Sign-in was not verified. Try again with the same account." });
                Synchronize();
            }
            catch (Exception) { } // A replaced or closed verification owns no visible state.
            finally { cancellation.Dispose(); }
        }
    }
    public void CancelVerification(string context)
    {
        if (context == state.ContextId && state.Phase == DeviceApprovalPhase.Verifying) work?.Cancel();
    }

    private void Failed(Exception failure, bool uncertain)
    {
        var problem = failure as PostboxException;
        if (failure is SignInException)
        {
            Publish(state with { Phase = DeviceApprovalPhase.Unavailable, ProofId = DeviceApprovalState.NewId(), Challenge = null,
                Message = uncertain ? "Your session ended. The decision is not confirmed; sign in again and check this request." : "Sign in again to continue." });
            return;
        }
        if (problem?.Code == "step_up_required")
        {
            Publish(state with
            {
                Phase = problem.StrongSignIn is null ? DeviceApprovalPhase.Unavailable : DeviceApprovalPhase.Review,
                ProofId = DeviceApprovalState.NewId(), Challenge = problem.StrongSignIn,
                Message = problem.StrongSignIn is null ? "The server did not provide a supported verification method."
                    : "Verify your sign-in, then review and decide this request again."
            });
            return;
        }
        if (problem?.StatusCode == 429) { rateOwner = bound?.OwnerKey; deadline = Now + Math.Clamp(problem.RetryAfter ?? 30, 1, 3600); }
        var phase = problem?.Code switch
        {
            "pairing_expired" => DeviceApprovalPhase.Expired,
            "forbidden" or "human_required" or "delegation_required" or "feature_disabled" or "step_up_unavailable" or "pairing_not_found"
                => DeviceApprovalPhase.Unavailable,
            _ => uncertain ? DeviceApprovalPhase.Unconfirmed : DeviceApprovalPhase.Idle
        };
        var message = problem switch
        {
            { StatusCode: 429 } => "Too many attempts. Wait before checking again.",
            { Code: "pairing_expired" } => "This request expired. Start again on the machine.",
            { Code: "pairing_not_found" } => "No request was found for this code. Check it on the machine.",
            { Code: "step_up_unavailable" } => "Strong sign-in is unavailable. Ask your administrator to configure it.",
            _ when phase == DeviceApprovalPhase.Unavailable => "Your account cannot decide this request, or the feature is unavailable.",
            _ when uncertain => "The decision is not confirmed. Check the request's status before deciding again.",
            _ => "The request could not be loaded. Try again."
        };
        Publish(state with
        {
            Phase = phase, ProofId = DeviceApprovalState.NewId(), Challenge = null, RetryAt = deadline, Message = message,
            Preview = problem?.StatusCode is 403 or 404 ? null : state.Preview
        });
    }
}
