using System.Text.Json;
using System.Text.Json.Nodes;

namespace Pigeonpost.Core;

public enum CallMode { Hidden, Mini, Full }

public sealed record CallPageMessage(string Type, CallMode Mode = CallMode.Hidden, string? Text = null);

// The contract between the app and the web call client's native page (site-inbox/native-call.js).
// The page posts {type: ready|mode|renew|toast}; the app answers through PigeonpostNative.*.
// Everything the app puts into a script is JSON-encoded, never concatenated as raw text.
public static class CallBridge
{
    public static readonly Uri ProductionPage = new("https://inbox.pigeonpost.dev/native-call.html?v=20261019-native");

    public static CallPageMessage? Parse(string? json)
    {
        if (string.IsNullOrWhiteSpace(json)) return null;
        try
        {
            if (JsonNode.Parse(json) is not JsonObject message || message["type"] is not JsonValue typeValue
                || !typeValue.TryGetValue<string>(out var type)) return null;
            return type switch
            {
                "ready" or "renew" => new CallPageMessage(type),
                "mode" => new CallPageMessage(type, ParseMode(message["mode"] is JsonValue m && m.TryGetValue<string>(out var mode) ? mode : null)),
                "toast" => message["text"] is JsonValue t && t.TryGetValue<string>(out var text) && !string.IsNullOrWhiteSpace(text)
                    ? new CallPageMessage(type, Text: text.Length > 300 ? text[..300] : text) : null,
                _ => null
            };
        }
        catch (JsonException) { return null; }
    }

    public static CallMode ParseMode(string? mode) => mode switch { "full" => CallMode.Full, "mini" => CallMode.Mini, _ => CallMode.Hidden };

    public static string ConfigureScript(string? token, Mailbox mailbox, IEnumerable<Conversation> conversations)
    {
        var known = new JsonObject();
        foreach (var conversation in conversations) known[conversation.Peer] = conversation.Name;
        var options = new JsonObject
        {
            ["token"] = token,
            ["mailbox"] = new JsonObject { ["address"] = mailbox.Address, ["handle"] = mailbox.Handle },
            ["known"] = known
        };
        return $"window.PigeonpostNative && PigeonpostNative.configure({options.ToJsonString()})";
    }

    public static string RenewedScript(string? token) => $"window.PigeonpostNative && PigeonpostNative.renewed({Json(token)})";

    public static string CallScript(string peer, bool video) =>
        $"window.PigeonpostNative && PigeonpostNative.call({Json(peer)}, {Json(video ? "video" : "audio")})";

    public const string SignOutScript = "window.PigeonpostNative && PigeonpostNative.signOut()";

    // The call page itself, and nothing else, may load in the call view.
    public static bool IsPage(Uri? candidate, Uri page) =>
        candidate is not null && candidate.IsAbsoluteUri && SameOrigin(candidate, page)
        && string.Equals(candidate.AbsolutePath, page.AbsolutePath, StringComparison.Ordinal);

    public static bool SameOrigin(Uri? candidate, Uri page) =>
        candidate is not null && candidate.IsAbsoluteUri
        && string.Equals(candidate.Scheme, page.Scheme, StringComparison.OrdinalIgnoreCase)
        && string.Equals(candidate.IdnHost, page.IdnHost, StringComparison.OrdinalIgnoreCase)
        && candidate.Port == page.Port;

    // Only ordinary web links leave the call view, and they open in the person's browser.
    public static bool IsExternalLink(Uri? candidate) =>
        candidate is not null && candidate.IsAbsoluteUri && (candidate.Scheme == Uri.UriSchemeHttps || candidate.Scheme == Uri.UriSchemeHttp);

    private static string Json(string? value) => JsonSerializer.Serialize(value);
}
