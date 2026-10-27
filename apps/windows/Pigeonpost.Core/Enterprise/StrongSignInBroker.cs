namespace Pigeonpost.Core.Enterprise;

// Fresh verification through the system browser: a request-owned IPv4 loopback return, the fixed
// trusted issuer and same-person adoption by AccountSession. It never approves anything itself.
public static class StrongSignInBroker
{
    public static async Task<AuthorizationLease> VerifyAsync(AccountSession session, StrongSignInRequirement requirement,
        AuthorizationLease expected, Func<Uri, Task<bool>> openSystemBrowser, CancellationToken ct = default)
    {
        using var listener = new LoopbackSignInReturn();
        using var request = session.BeginStrongSignIn(requirement, expected, listener.RedirectUri, ct);
        Task<Uri>? waiting = null;
        try
        {
            waiting = listener.WaitForCallbackAsync(request, ct);
            bool opened;
            try { opened = await openSystemBrowser(request.AuthorizationUri); }
            catch (Exception ex) when (ex is not OperationCanceledException) { opened = false; }
            if (!opened) throw new SignInException("Could not open your browser to verify your sign-in. Your current sign-in is unchanged.");
            var callback = await waiting;
            return await session.CompleteStrongSignInAsync(request, callback, ct);
        }
        catch
        {
            session.CancelStrongSignIn(request);
            listener.Dispose();
            if (waiting is not null) { try { await waiting; } catch { } }
            throw;
        }
    }
}
