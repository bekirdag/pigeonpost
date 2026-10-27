#if UI_TESTS
using System.Net.Http;
using System.Text.Json;
using Pigeonpost.Core;
using Pigeonpost.Core.Enterprise;

namespace Pigeonpost.Desktop;

// The synthetic signed-in person for the approval UI test. Rotate models a verified same-person
// adoption: the old grant is retired and every grant-bound view must rebind deliberately.
internal sealed class NativeApprovalAccount
{
    public AuthorizationLease Lease { get; private set; } = new();
    public string Owner { get; } = new('b', 64);
    public int Rotations { get; private set; }
    public AuthorizationLease Rotate() { var old = Lease; Lease = new(); old.Retire(); Rotations++; return Lease; }
}

// Synthetic machine-approval service: no HTTP, public authentication, real pairing or agent.
// ABCD-EFGH creates a new agent, needs fresh verification first, then loses one committed reply.
// WXYZ-2345 is an existing agent that the person denies.
internal sealed class NativeApprovalFixture(NativeApprovalAccount account, string statePath) : IDevicePairingApi
{
    private const string Agent = "/k/rywv63mb6h0hhrpfjewrrttspw";
    private int resolves, approves, denies, verifications, foreignActors;
    private readonly Dictionary<string, string> states = new(StringComparer.Ordinal) { ["ABCDEFGH"] = "pending", ["WXYZ2345"] = "pending" };
    private readonly long requested = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
    private AuthorizationLease? verifiedGrant;
    public AuthorizationLease? AcceptedAuthorization => account.Lease;

    private static string Key(DevicePairingCode code) => code.DisplayValue.Replace("-", "");
    private void Admit(CancellationToken ct)
    {
        ct.ThrowIfCancellationRequested();
        if (AuthorizationContext.Current is not { IsActive: true } actor || !actor.SharesAuthorization(account.Lease)) { foreignActors++; Save(); throw new AuthorizationRetiredException(); }
    }
    private DevicePairingPreview Preview(string key) => key == "ABCDEFGH"
        ? new("pair_new", new("org_fixture", "Example Organization"), "/example/build", "123 456", ["mailbox"], requested, requested + 900,
            CreatesAgent: true, Label: "Build server", IpCountry: "TR", CanApprove: true, State: states[key])
        : new("pair_existing", new("org_fixture", "Example Organization"), "/example/support", "654 321", ["mailbox"], requested, requested + 900,
            Address: Agent, Label: "Support laptop", IpCountry: "DE", CanApprove: true, State: states[key]);

    public async Task<DevicePairingPreview> ResolveDevicePairingAsync(DevicePairingCode code, CancellationToken ct)
    {
        await Task.Delay(250, ct); Admit(ct); resolves++; Save();
        return states.ContainsKey(Key(code)) ? Preview(Key(code)) : throw new PostboxException(404, "pairing_not_found");
    }
    public async Task ApproveDevicePairingAsync(DevicePairingPreview preview, DevicePairingCode code, CancellationToken ct)
    {
        await Task.Delay(250, ct); Admit(ct); approves++; Save();
        if (Key(code) != "ABCDEFGH" || preview.PairingId != "pair_new") throw new PostboxException(409, "pairing_not_actionable");
        // A fresh strong sign-in is required, as the server signals with step_up_required.
        if (verifiedGrant is null || !ReferenceEquals(verifiedGrant, account.Lease))
            throw new PostboxException(401, "step_up_required", strongSignIn: StrongSignInRequirement.Create(["mfa"], 600));
        states["ABCDEFGH"] = "approved"; Save();
        throw new HttpRequestException("Synthetic committed approval with a lost reply.");
    }
    public async Task DenyDevicePairingAsync(DevicePairingPreview preview, DevicePairingCode code, CancellationToken ct)
    {
        await Task.Delay(250, ct); Admit(ct); denies++;
        if (states[Key(code)] == "pending") states[Key(code)] = "denied";
        Save();
    }
    // The fixture's verification step: account rotation stands in for AccountSession's verified adoption.
    public AuthorizationLease Verify(StrongSignInRequirement requirement, AuthorizationLease expected)
    {
        if (!ReferenceEquals(expected, account.Lease) || requirement.RequestedAcr != "mfa") throw new StrongSignInException();
        verifications++; verifiedGrant = account.Rotate(); Save(); return verifiedGrant;
    }
    private void Save()
    {
        var state = new { resolves, approves, denies, verifications, foreignActors, rotations = account.Rotations, states };
        var temporary = statePath + ".new"; File.WriteAllText(temporary, JsonSerializer.Serialize(state)); File.Move(temporary, statePath, overwrite: true);
    }
}
#endif
