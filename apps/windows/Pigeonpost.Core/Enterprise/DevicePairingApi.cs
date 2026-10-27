namespace Pigeonpost.Core.Enterprise;

public interface IDevicePairingApi
{
    AuthorizationLease? AcceptedAuthorization { get; }
    Task<DevicePairingPreview> ResolveDevicePairingAsync(DevicePairingCode code, CancellationToken ct);
    Task ApproveDevicePairingAsync(DevicePairingPreview preview, DevicePairingCode code, CancellationToken ct);
    // A denial acknowledgment requires a fresh resolve to know the final state.
    Task DenyDevicePairingAsync(DevicePairingPreview preview, DevicePairingCode code, CancellationToken ct);
}
