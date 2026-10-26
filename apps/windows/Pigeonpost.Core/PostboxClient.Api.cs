using System.Text.Json;

namespace Pigeonpost.Core;

// Shared plumbing for the Phase 1b and Phase 3 routes. Every request names the acting mailbox both
// as `?identity=` and as `x-pigeonpost-identity`, because routes differ in which one they read;
// device-scoped group routes add `x-pigeonpost-device`.
public sealed partial class PostboxClient
{
    public Uri Endpoint => endpoint;

    internal async Task<T> GetJsonAsync<T>(string path, string? identity, CancellationToken cancellationToken, string? device = null)
    {
        using var response = await RequestAsync(HttpMethod.Get, Scoped(path, identity), null, cancellationToken,
            identity: identity, device: device).ConfigureAwait(false);
        return await DecodeAsync<T>(response, cancellationToken).ConfigureAwait(false);
    }

    internal async Task<T> SendJsonAsync<T>(HttpMethod method, string path, object? body, string? identity, CancellationToken cancellationToken, string? device = null)
    {
        using var response = await RequestAsync(method, Scoped(path, identity),
            body is null ? null : JsonSerializer.Serialize(body, PostboxJson.Options), cancellationToken, identity: identity, device: device).ConfigureAwait(false);
        return await DecodeAsync<T>(response, cancellationToken).ConfigureAwait(false);
    }

    internal async Task SendAsync(HttpMethod method, string path, object? body, string? identity, CancellationToken cancellationToken, string? device = null)
    {
        using var response = await RequestAsync(method, Scoped(path, identity),
            body is null ? null : JsonSerializer.Serialize(body, PostboxJson.Options), cancellationToken, identity: identity, device: device).ConfigureAwait(false);
    }

    internal async Task<T> SendBytesAsync<T>(HttpMethod method, string path, byte[] bytes, string mediaType, string? identity, CancellationToken cancellationToken, string? device = null)
    {
        using var response = await RequestAsync(method, Scoped(path, identity), null, cancellationToken, bytes, identity,
            transfer: true, mediaType: mediaType, device: device).ConfigureAwait(false);
        return await DecodeAsync<T>(response, cancellationToken).ConfigureAwait(false);
    }

    internal async Task<byte[]> GetBytesAsync(string path, string? identity, CancellationToken cancellationToken, string? device = null, int maxBytes = MaxAttachmentBytes * 2)
    {
        using var response = await RequestAsync(HttpMethod.Get, Scoped(path, identity), null, cancellationToken,
            identity: identity, transfer: true, device: device).ConfigureAwait(false);
        await response.Content.LoadIntoBufferAsync(maxBytes, cancellationToken).ConfigureAwait(false);
        var bytes = await response.Content.ReadAsByteArrayAsync(cancellationToken).ConfigureAwait(false);
        CheckResponseAuthorization(response, cancellationToken);
        return bytes;
    }

    // The calls service shares the inbox's origin (inbox.pigeonpost.dev/calls/v1). The member token
    // goes there and nowhere else: any other host is refused.
    internal async Task<T> GetCallsJsonAsync<T>(Uri absolute, CancellationToken cancellationToken)
    {
        if (!CallBridge.IsCallsService(absolute, endpoint))
            throw new ArgumentException("Not the calls service.", nameof(absolute));
        using var request = new HttpRequestMessage(HttpMethod.Get, absolute);
        var owner = new RequestAuthorization(tokens);
        owner.Check(cancellationToken);
        var token = await tokens.GetTokenAsync(cancellationToken).ConfigureAwait(false);
        owner.Check(cancellationToken);
        request.Options.Set(AuthorizationKey, owner);
        request.Headers.Authorization = new("Bearer", token);
        request.Headers.Accept.Add(new("application/json"));
        using var response = await http.SendAsync(request, cancellationToken).ConfigureAwait(false);
        response.RequestMessage = request;
        owner.Check(cancellationToken);
        if (!response.IsSuccessStatusCode) throw new PostboxException((int)response.StatusCode, "calls_unavailable");
        return await DecodeAsync<T>(response, cancellationToken).ConfigureAwait(false);
    }

    private static string Scoped(string path, string? identity) => identity is null ? path
        : path + (path.Contains('?') ? "&" : "?") + "identity=" + Uri.EscapeDataString(identity);

    // What this server enables for the signed-in account. A server without the route (or an
    // error) means none of the optional features: the app then shows only what it always had.
    public async Task<ServerFeatures> GetCapabilitiesAsync(CancellationToken cancellationToken)
    {
        try
        {
            var body = await ReadAsync<CapabilitiesResponse>("/v1/capabilities", cancellationToken).ConfigureAwait(false);
            return new ServerFeatures(new HashSet<string>(body.Features ?? [], StringComparer.Ordinal));
        }
        catch (PostboxException ex) when (ex.StatusCode is 404 or 501 or >= 500) { return ServerFeatures.None; }
    }

    private sealed record CapabilitiesResponse(IReadOnlyList<string>? Features);
}

public sealed record ServerFeatures(IReadOnlySet<string> Names)
{
    public const string MessageFiltering = "message_filtering";
    public const string CallAdmission = "call_admission";
    public const string CallHistory = "call_history";
    public const string Avatars = "avatars";
    public const string Groups = "groups";
    public const string Disappearing = "disappearing";

    public static ServerFeatures None { get; } = new(new HashSet<string>(StringComparer.Ordinal));
    public bool Has(string name) => Names.Contains(name);
}
