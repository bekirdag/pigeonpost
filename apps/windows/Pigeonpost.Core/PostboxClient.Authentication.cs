using System.Net;
using System.Text.Json;

namespace Pigeonpost.Core;

public sealed partial class PostboxClient
{
    private static async Task<PostboxException> ReadProblemAsync(HttpResponseMessage response,
        RequestAuthorization owner, CancellationToken cancellationToken)
    {
        owner.Check(cancellationToken);
        string? header = null;
        if (response.Headers.TryGetValues("WWW-Authenticate", out var headers))
        {
            foreach (var value in headers)
            {
                if ((header?.Length ?? 0) + value.Length > 2048) { header = new string('x', 2049); break; }
                header = header is null ? value : header + ", " + value;
            }
        }
        var signalled = response.StatusCode == HttpStatusCode.Unauthorized && StrongSignInRequirement.SignalsChallenge(header);
        JsonElement? detail = null;
        try
        {
            await response.Content.LoadIntoBufferAsync(64 * 1024, cancellationToken).ConfigureAwait(false);
            var bytes = await response.Content.ReadAsByteArrayAsync(cancellationToken).ConfigureAwait(false);
            owner.Check(cancellationToken);
            if (bytes.Length > 0)
            {
                using var document = JsonDocument.Parse(bytes);
                if (document.RootElement.ValueKind != JsonValueKind.Object
                    || document.RootElement.EnumerateObject().Select(p => p.Name).Distinct(StringComparer.Ordinal).Count()
                        != document.RootElement.EnumerateObject().Count()) throw new JsonException();
                detail = document.RootElement.Clone();
            }
        }
        catch (Exception ex) when (ex is JsonException or NotSupportedException or InvalidOperationException or HttpRequestException)
        {
            owner.Check(cancellationToken);
            if (!signalled) throw new PostboxException((int)response.StatusCode, "bad_response");
        }
        owner.Check(cancellationToken);
        string? code = null;
        if (detail is { } body && body.TryGetProperty("error", out var error) && error.ValueKind == JsonValueKind.String)
            code = error.GetString();
        if (signalled || response.StatusCode == HttpStatusCode.Unauthorized && code is "step_up_required" or "insufficient_user_authentication")
            return new((int)response.StatusCode, "step_up_required", detail, StrongSignInRequirement.Parse(detail, header));
        return new((int)response.StatusCode, response.StatusCode == HttpStatusCode.Unauthorized ? "unauthorized" : code, detail);
    }
}
