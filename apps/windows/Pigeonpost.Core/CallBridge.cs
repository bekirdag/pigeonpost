using System.Text.Json;
using System.Text.Json.Nodes;

namespace Pigeonpost.Core;

public enum CallMode { Hidden, Mini, Full }

public sealed record CallPageMessage(string Type, CallMode Mode = CallMode.Hidden, string? Text = null, bool Ringing = false,
    CallRing? Ring = null, string? Room = null, string? State = null, Guid? Operation = null, string? ErrorCode = null);

// The contract between the app and the web call client's native page (site-inbox/native-call.js).
// The page posts {type: ready|mode|renew|toast|ring|ring_ended|call}; the app answers through
// PigeonpostNative.*. (ring_card comes from the app's own observer script below.)
// Everything the app puts into a script is JSON-encoded, never concatenated as raw text.
public static partial class CallBridge
{
    public static readonly Uri ProductionPage = new("https://inbox.pigeonpost.dev/native-call.html?v=20261027-lobby2");

    public static CallPageMessage? Parse(string? json)
    {
        if (string.IsNullOrWhiteSpace(json) || json.Length > 16_384) return null;
        try
        {
            if (JsonNode.Parse(json) is not JsonObject message || message["type"] is not JsonValue typeValue
                || !typeValue.TryGetValue<string>(out var type)) return null;
            return type switch
            {
                "ready" or "renew" => new CallPageMessage(type),
                "scheduled_result" => ParseScheduled(message),
                "ring_card" => new CallPageMessage(type, Ringing: message["ringing"] is JsonValue r && r.TryGetValue<bool>(out var ringing) && ringing),
                // Phase 1c: a ring arrived on the event stream (docs/call-alerting.md §5).
                "ring" => CallRing.FromPage(message) is { } ring ? new CallPageMessage(type, Ring: ring, Room: ring.Room) : null,
                "ring_ended" => CallRing.Text(message["room"]) is { } ended && CallRing.ValidRoom(ended) ? new CallPageMessage(type, Room: ended) : null,
                "call" => CallRing.Text(message["state"]) is "connecting" or "active" or "ended"
                    ? new CallPageMessage(type, State: CallRing.Text(message["state"]),
                        Room: CallRing.Text(message["room"]) is { } callRoom && CallRing.ValidRoom(callRoom) ? callRoom : null)
                    : null,
                "mode" => new CallPageMessage(type, ParseMode(message["mode"] is JsonValue m && m.TryGetValue<string>(out var mode) ? mode : null)),
                "toast" => message["text"] is JsonValue t && t.TryGetValue<string>(out var text) && !string.IsNullOrWhiteSpace(text)
                    ? new CallPageMessage(type, Text: text.Length > 300 ? text[..300] : text) : null,
                _ => null
            };
        }
        catch (Exception error) when (error is JsonException or ArgumentException or InvalidOperationException) { return null; }
    }

    // Windows only, added by the app: WebView2 in WinUI cannot show the inbox through a transparent
    // page, so while the page only rings (no call yet) the app shrinks the view to the ring card.
    public const string RingObserverScript = """
        (() => {
          let last = null;
          const post = () => {
            const ring = document.getElementById("call-ring"), ui = document.getElementById("call-ui");
            const ringing = Boolean(ring && !ring.hidden && !(ui && !ui.hidden));
            if (ringing === last) return;
            last = ringing;
            try { window.chrome.webview.postMessage({ type: "ring_card", ringing }); } catch (_) {}
          };
          const start = () => {
            new MutationObserver(post).observe(document.body, { subtree: true, childList: true, attributes: true, attributeFilter: ["hidden"] });
            post();
          };
          if (document.readyState === "loading") document.addEventListener("DOMContentLoaded", start); else start();
        })();
        """;

    public static CallMode ParseMode(string? mode) => mode switch { "full" => CallMode.Full, "mini" => CallMode.Mini, _ => CallMode.Hidden };

    // nativeRing: the app shows incoming calls itself (a toast) and the page shows no ring screen and
    // plays no ringtone. Pages from before Phase 1c ignore it and ring as they always did.
    public static string ConfigureScript(string? token, Mailbox mailbox, IEnumerable<Conversation> conversations, bool nativeRing = false)
    {
        var known = new JsonObject();
        foreach (var conversation in conversations) known[conversation.Peer] = conversation.Name;
        var options = new JsonObject
        {
            ["token"] = token,
            ["mailbox"] = new JsonObject { ["address"] = mailbox.Address, ["handle"] = mailbox.Handle },
            ["known"] = known,
            ["nativeRing"] = nativeRing
        };
        return $"window.PigeonpostNative && PigeonpostNative.configure({options.ToJsonString()})";
    }

    public static string RenewedScript(string? token) => $"window.PigeonpostNative && PigeonpostNative.renewed({Json(token)})";

    // Phase 3b: start or join a group's call through the page's own group calls (calls.js
    // placeGroup/joinGroup). The calls service rechecks membership with the postbox.
    public static string GroupCallScript(string groupId, string name, bool video, bool join) =>
        $"window.PigeonpostCalls && PigeonpostCalls.{(join ? "joinGroup" : "placeGroup")}({Json(video ? "video" : "audio")}, {{ id: {Json(groupId)}, name: {Json(name)} }})";

    public static string CallScript(string peer, bool video) =>
        $"window.PigeonpostNative && PigeonpostNative.call({Json(peer)}, {Json(video ? "video" : "audio")})";

    // Answer or decline a ring, with its ring token when a push supplied one. Guarded so a page from
    // before Phase 1c, without these calls, is left alone.
    public static string AnswerScript(string room, string? ringToken) =>
        $"window.PigeonpostNative && PigeonpostNative.answer && PigeonpostNative.answer({Json(room)}, {Json(ringToken)})";

    public static string DeclineScript(string room, string? ringToken) =>
        $"window.PigeonpostNative && PigeonpostNative.decline && PigeonpostNative.decline({Json(room)}, {Json(ringToken)})";

    public const string SignOutScript = "window.PigeonpostNative && PigeonpostNative.signOut()";

    // The call page itself, and nothing else, may load in the call view.
    public static bool IsPage(Uri? candidate, Uri page) =>
        candidate is not null && candidate.IsAbsoluteUri && SameOrigin(candidate, page)
        && string.IsNullOrEmpty(candidate.UserInfo)
        && string.Equals(candidate.AbsolutePath, page.AbsolutePath, StringComparison.Ordinal);

    public static bool SameOrigin(Uri? candidate, Uri page) =>
        candidate is not null && candidate.IsAbsoluteUri
        && string.Equals(candidate.Scheme, page.Scheme, StringComparison.OrdinalIgnoreCase)
        && string.Equals(candidate.IdnHost, page.IdnHost, StringComparison.OrdinalIgnoreCase)
        && candidate.Port == page.Port;

    public static bool IsCallsService(Uri? candidate, Uri postboxEndpoint) =>
        candidate is { IsAbsoluteUri: true } && string.IsNullOrEmpty(candidate.UserInfo) && string.IsNullOrEmpty(candidate.Fragment)
        && candidate.AbsolutePath.StartsWith("/calls/v1/", StringComparison.Ordinal)
        && (candidate.Scheme == Uri.UriSchemeHttps && candidate.IdnHost == "inbox.pigeonpost.dev" && candidate.Port == 443
            || candidate.IsLoopback && postboxEndpoint.IsLoopback && SameOrigin(candidate, postboxEndpoint));

    // Only ordinary web links leave the call view, and they open in the person's browser.
    public static bool IsExternalLink(Uri? candidate) =>
        candidate is not null && candidate.IsAbsoluteUri && (candidate.Scheme == Uri.UriSchemeHttps || candidate.Scheme == Uri.UriSchemeHttp);

    private static string Json(string? value) => JsonSerializer.Serialize(value);
}
