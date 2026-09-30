using System.Net;
using System.Text.Json;

namespace Pigeonpost.Core;

public interface IRefreshTokenStore
{
    string? Load();
    void Save(string token);
    void Clear();
}

public sealed class SignInException(string message) : Exception(message);
public sealed record DeviceSignIn(string DeviceCode, string UserCode, Uri VerificationUri, int ExpiresIn, int Interval);

// Callers must disable automatic HTTP redirects. Only refresh tokens enter the operating-system vault.
public sealed class AccountSession(HttpClient http, IRefreshTokenStore store, Uri? issuer = null) : IAccessTokenProvider
{
    public const string ClientId = "pigeonpost-windows";
    private readonly Uri issuer = ValidateIssuer(issuer ?? new Uri("https://auth.pigeonpost.dev/realms/pigeonpost-prod/"));
    private readonly SemaphoreSlim gate = new(1, 1);
    private string? accessToken;
    private string? refreshToken;
    private DateTimeOffset expiresAt;
    private int generation;

    public async Task<bool> RestoreAsync(CancellationToken cancellationToken)
    {
        await gate.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            refreshToken = store.Load();
            if (string.IsNullOrEmpty(refreshToken)) return false;
            await RenewAsync(cancellationToken).ConfigureAwait(false);
            return true;
        }
        finally { gate.Release(); }
    }

    public async Task<DeviceSignIn> BeginSignInAsync(CancellationToken cancellationToken)
    {
        using var result = await PostAsync("auth/device", new()
        {
            ["client_id"] = ClientId, ["scope"] = "openid profile offline_access"
        }, cancellationToken).ConfigureAwait(false);
        var root = result.RootElement;
        CheckError(root);
        var uriText = Text(root, "verification_uri_complete") ?? Text(root, "verification_uri");
        if (!Uri.TryCreate(uriText, UriKind.Absolute, out var uri) || uri.Scheme != issuer.Scheme
            || uri.Authority != issuer.Authority || !uri.AbsolutePath.StartsWith(issuer.AbsolutePath, StringComparison.Ordinal))
            throw new SignInException("The sign-in service returned an invalid browser address.");
        return new DeviceSignIn(Required(root, "device_code"), Required(root, "user_code"), uri,
            Math.Clamp(Number(root, "expires_in", 600), 1, 1800), Math.Clamp(Number(root, "interval", 5), 1, 60));
    }

    public async Task CompleteSignInAsync(DeviceSignIn request, CancellationToken cancellationToken)
    {
        var attempt = Volatile.Read(ref generation);
        var deadline = DateTimeOffset.UtcNow.AddSeconds(request.ExpiresIn);
        var interval = request.Interval;
        while (DateTimeOffset.UtcNow < deadline)
        {
            await Task.Delay(TimeSpan.FromSeconds(interval), cancellationToken).ConfigureAwait(false);
            if (DateTimeOffset.UtcNow >= deadline) break;
            using var result = await PostAsync("token", new()
            {
                ["client_id"] = ClientId, ["grant_type"] = "urn:ietf:params:oauth:grant-type:device_code", ["device_code"] = request.DeviceCode
            }, cancellationToken).ConfigureAwait(false);
            var error = Text(result.RootElement, "error");
            if (error == "authorization_pending") continue;
            if (error == "slow_down") { interval = Math.Min(interval + 5, 60); continue; }
            CheckError(result.RootElement);
            await gate.WaitAsync(cancellationToken).ConfigureAwait(false);
            try
            {
                cancellationToken.ThrowIfCancellationRequested();
                if (attempt != generation) throw new OperationCanceledException(cancellationToken);
                Accept(result.RootElement, cancellationToken, requireRefresh: true);
                return;
            }
            finally { gate.Release(); }
        }
        throw new SignInException("This sign-in code expired. Start again for a new code.");
    }

    public async Task<string> GetTokenAsync(CancellationToken cancellationToken)
    {
        await gate.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            if (accessToken is null || DateTimeOffset.UtcNow >= expiresAt) await RenewAsync(cancellationToken).ConfigureAwait(false);
            return accessToken!;
        }
        finally { gate.Release(); }
    }

    public async Task<string> RefreshTokenAsync(CancellationToken cancellationToken)
    {
        await gate.WaitAsync(cancellationToken).ConfigureAwait(false);
        try { await RenewAsync(cancellationToken).ConfigureAwait(false); return accessToken!; }
        finally { gate.Release(); }
    }

    public async Task SignOutAsync(CancellationToken cancellationToken)
    {
        Interlocked.Increment(ref generation);
        await gate.WaitAsync(cancellationToken).ConfigureAwait(false);
        try
        {
            // If vault deletion fails, keep the session visible so the user can retry sign-out.
            store.Clear();
            accessToken = refreshToken = null;
            expiresAt = default;
        }
        finally { gate.Release(); }
    }

    private async Task RenewAsync(CancellationToken cancellationToken)
    {
        if (string.IsNullOrEmpty(refreshToken)) throw new SignInException("Sign in to connect your Pigeonpost account.");
        using var result = await PostAsync("token", new()
        {
            ["client_id"] = ClientId, ["grant_type"] = "refresh_token", ["refresh_token"] = refreshToken
        }, cancellationToken).ConfigureAwait(false);
        if (Text(result.RootElement, "error") == "invalid_grant")
        {
            store.Clear();
            accessToken = refreshToken = null;
            expiresAt = default;
        }
        CheckError(result.RootElement);
        Accept(result.RootElement, cancellationToken);
    }

    private void Accept(JsonElement response, CancellationToken cancellationToken, bool requireRefresh = false)
    {
        var access = Required(response, "access_token");
        var refresh = Text(response, "refresh_token") ?? (requireRefresh ? null : refreshToken);
        if (string.IsNullOrEmpty(refresh)) throw new SignInException("Sign-in did not provide a renewable session. Please try again.");
        cancellationToken.ThrowIfCancellationRequested();
        store.Save(refresh);
        refreshToken = refresh;
        accessToken = access;
        expiresAt = DateTimeOffset.UtcNow.AddSeconds(Math.Max(0, Number(response, "expires_in", 300) - 30));
    }

    private async Task<JsonDocument> PostAsync(string suffix, Dictionary<string, string> fields, CancellationToken cancellationToken)
    {
        using var request = new HttpRequestMessage(HttpMethod.Post, new Uri(issuer, "protocol/openid-connect/" + suffix))
        { Content = new FormUrlEncodedContent(fields) };
        using var response = await http.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, cancellationToken).ConfigureAwait(false);
        if (response.StatusCode is not HttpStatusCode.OK and not HttpStatusCode.BadRequest and not HttpStatusCode.Unauthorized)
            throw new SignInException("The sign-in service is unavailable. Please try again.");
        try
        {
            await response.Content.LoadIntoBufferAsync(128 * 1024, cancellationToken).ConfigureAwait(false);
            var result = JsonDocument.Parse(await response.Content.ReadAsStringAsync(cancellationToken).ConfigureAwait(false));
            if (result.RootElement.ValueKind != JsonValueKind.Object) { result.Dispose(); throw new JsonException(); }
            if (!response.IsSuccessStatusCode && Text(result.RootElement, "error") is null)
            { result.Dispose(); throw new JsonException(); }
            return result;
        }
        catch (JsonException) { throw new SignInException("The sign-in service returned an unreadable response."); }
    }

    private static string? Text(JsonElement root, string name) => root.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String ? value.GetString() : null;
    private static string Required(JsonElement root, string name) => Text(root, name) is { Length: > 0 } value ? value : throw new SignInException("The sign-in response was incomplete. Please try again.");
    private static int Number(JsonElement root, string name, int fallback) => root.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.Number && value.TryGetInt32(out var number) ? number : fallback;
    private static void CheckError(JsonElement root)
    {
        if (Text(root, "error") is not { } error) return;
        throw new SignInException(error switch
        {
            "access_denied" => "Sign-in was declined. You can start again when ready.",
            "expired_token" => "This sign-in code expired. Start again for a new code.",
            "invalid_grant" => "Your session expired. Sign in again.",
            _ => "Could not sign in. Please try again."
        });
    }
    private static Uri ValidateIssuer(Uri value) => value.IsAbsoluteUri && (value.Scheme == "https" || value.Scheme == "http" && value.IsLoopback)
        && value.AbsolutePath.EndsWith('/') ? value : throw new ArgumentException("Use an HTTPS issuer ending in / (HTTP is allowed only on loopback).", nameof(value));
}
