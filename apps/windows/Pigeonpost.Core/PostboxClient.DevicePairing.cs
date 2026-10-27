using System.Text.Json;
using Pigeonpost.Core.Enterprise;

namespace Pigeonpost.Core;

public sealed partial class PostboxClient : IDevicePairingApi
{
    AuthorizationLease? IDevicePairingApi.AcceptedAuthorization => AcceptedAuthorization;

    public async Task<DevicePairingPreview> ResolveDevicePairingAsync(DevicePairingCode code, CancellationToken ct)
    {
        using var operation = BindPairingAccount();
        var owner = new RequestAuthorization(tokens);
        var preview = await PairingRequestAsync<DevicePairingPreview>("/v1/device-pairings/resolve", code, ct).ConfigureAwait(false);
        owner.Check(ct);
        if (!preview.IsValid) throw UnverifiedPairing();
        return preview;
    }

    public async Task ApproveDevicePairingAsync(DevicePairingPreview preview, DevicePairingCode code, CancellationToken ct)
    {
        using var operation = BindPairingAccount();
        var owner = new RequestAuthorization(tokens);
        if (!preview.AllowsApproval(DateTimeOffset.UtcNow.ToUnixTimeSeconds())) throw new PostboxException(409, "pairing_not_actionable");
        var result = await PairingRequestAsync<PairingApproval>("/v1/device-pairings/" + preview.PairingId + "/approve", code, ct).ConfigureAwait(false);
        owner.Check(ct);
        if (!result.Approved || result.Handle != preview.Handle || result.Address is null
            || !MessageRecoveryScope.CanonicalMailbox(result.Address) || preview.Address is not null && preview.Address != result.Address)
            throw UnverifiedPairing();
    }

    public async Task DenyDevicePairingAsync(DevicePairingPreview preview, DevicePairingCode code, CancellationToken ct)
    {
        using var operation = BindPairingAccount();
        var owner = new RequestAuthorization(tokens);
        if (!preview.AllowsDecision(DateTimeOffset.UtcNow.ToUnixTimeSeconds())) throw new PostboxException(409, "pairing_not_actionable");
        var result = await PairingRequestAsync<PairingDenial>("/v1/device-pairings/" + preview.PairingId + "/deny", code, ct).ConfigureAwait(false);
        owner.Check(ct);
        if (!result.Denied) throw UnverifiedPairing();
    }

    private AuthorizationContext BindPairingAccount()
    {
        if (AcceptedAuthorization is not { IsActive: true } root) throw new AuthorizationRetiredException();
        return AuthorizationContext.Capture(root);
    }

    private async Task<T> PairingRequestAsync<T>(string path, DevicePairingCode code, CancellationToken ct)
    {
        var owner = new RequestAuthorization(tokens); owner.Check(ct);
        using var response = await RequestAsync(HttpMethod.Post, path,
            JsonSerializer.Serialize(new { user_code = code.DisplayValue }, PostboxJson.Options), ct).ConfigureAwait(false);
        try
        {
            await response.Content.LoadIntoBufferAsync(1024 * 1024, ct).ConfigureAwait(false);
            var bytes = await response.Content.ReadAsByteArrayAsync(ct).ConfigureAwait(false); owner.Check(ct);
            using var document = JsonDocument.Parse(bytes);
            var body = document.RootElement;
            if (body.ValueKind != JsonValueKind.Object
                || body.EnumerateObject().Select(p => p.Name).Distinct(StringComparer.Ordinal).Count() != body.EnumerateObject().Count())
                throw UnverifiedPairing();
            var result = body.Deserialize<T>(PostboxJson.Options) ?? throw UnverifiedPairing();
            owner.Check(ct); return result;
        }
        catch (Exception error) when (error is JsonException or NotSupportedException or HttpRequestException)
        { owner.Check(ct); throw UnverifiedPairing(); }
    }
    private static PostboxException UnverifiedPairing() => new(200, "bad_response");
    private sealed record PairingApproval(bool Approved = false, string? Address = null, string? Handle = null);
    private sealed record PairingDenial(bool Denied = false);
}
