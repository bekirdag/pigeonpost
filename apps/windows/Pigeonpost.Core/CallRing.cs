using System.Net.Http.Json;
using System.Text;
using System.Text.Json.Nodes;
using System.Text.RegularExpressions;

namespace Pigeonpost.Core;

// Incoming-call alerting (docs/call-alerting.md, Phase 1c): the ring the call page reports, the
// Windows toast that shows it, the toast's launch arguments, and which rings are live.
//
// Everything here comes from outside the app (the page, a WNS push, toast activation), so every
// value is validated before it reaches a toast, a script or a URL.
public sealed partial record CallRing(string Room, string Kind, string? Code, string? FromAddress, string? FromHandle, string? To, string? RingToken = null)
{
    public bool Video => Kind == "video";
    public string? FromKey => FromHandle ?? FromAddress;

    // {type:"ring", room, code, kind, from:{address,handle}, to} from the call page.
    public static CallRing? FromPage(JsonObject message)
    {
        if (!ValidRoom(Text(message["room"]))) return null;
        var from = message["from"] as JsonObject;
        return new CallRing(Text(message["room"])!, ParseKind(Text(message["kind"])), Clean(Text(message["code"]), 16),
            Address(Text(from?["address"])), Address(Text(from?["handle"])), Address(Text(message["to"])));
    }

    public static string ParseKind(string? kind) => kind == "video" ? "video" : "audio";

    public static bool ValidRoom(string? room) => room is not null && RoomPattern().IsMatch(room);
    public static bool ValidRingToken(string? token) => token is not null && TokenPattern().IsMatch(token);

    // Addresses and handles are /-paths of printable characters; anything else is dropped.
    private static string? Address(string? value)
    {
        var clean = Clean(value, 256);
        return clean is { Length: > 1 } && clean[0] == '/' && !clean.Any(char.IsWhiteSpace) ? clean : null;
    }

    internal static string? Text(JsonNode? node) => node is JsonValue value && value.TryGetValue<string>(out var text) ? text : null;

    // Strip control, format (bidi overrides, zero-width) and unpaired surrogate characters, which
    // could reorder or hide text on a call screen or make the toast XML invalid; then cut to length.
    public static string? Clean(string? value, int max)
    {
        if (string.IsNullOrEmpty(value)) return null;
        var builder = new StringBuilder(Math.Min(value.Length, max));
        for (var i = 0; i < value.Length && builder.Length < max; i++)
        {
            var c = value[i];
            if (char.IsHighSurrogate(c))
            {
                if (i + 1 < value.Length && char.IsLowSurrogate(value[i + 1]))
                {
                    if (builder.Length + 2 > max) break;
                    builder.Append(c).Append(value[++i]);
                }
                continue;
            }
            if (char.IsLowSurrogate(c)) continue;
            var category = char.GetUnicodeCategory(c);
            if (category is System.Globalization.UnicodeCategory.Control or System.Globalization.UnicodeCategory.Format
                or System.Globalization.UnicodeCategory.LineSeparator or System.Globalization.UnicodeCategory.ParagraphSeparator
                or System.Globalization.UnicodeCategory.OtherNotAssigned || c is '￾' or '￿') continue;
            builder.Append(c);
        }
        var text = builder.ToString().Trim();
        return text.Length == 0 ? null : text;
    }

    [GeneratedRegex("^[A-Za-z0-9_-]{10,64}$")] private static partial Regex RoomPattern();
    [GeneratedRegex("^[A-Za-z0-9_-]{20,128}$")] private static partial Regex TokenPattern();
}

public enum CallLaunchAction { Open, Answer, Decline }

// Toast launch arguments: `action=answer|decline&room=…&ring=…&kind=…`, URL-encoded (the WNS toast
// the postbox sends, and the app's own toast, which uses the same format so one parser serves both).
// `;` is also accepted as a separator, the Windows App SDK builder's own format.
public sealed record CallLaunch(CallLaunchAction Action, string Room, string? RingToken, string Kind)
{
    public static CallLaunch? Parse(string? arguments)
    {
        if (string.IsNullOrEmpty(arguments) || arguments.Length > 2048) return null;
        var values = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var pair in arguments.Split('&', ';'))
        {
            var equals = pair.IndexOf('=');
            if (equals <= 0) continue;
            string key, value;
            try { key = Uri.UnescapeDataString(pair[..equals]); value = Uri.UnescapeDataString(pair[(equals + 1)..]); }
            catch (UriFormatException) { return null; }
            if (!values.TryAdd(key, value)) return null; // A repeated key is ambiguous: refuse it.
        }
        // A toast for something other than a call (a new-message alert) has no room.
        if (!values.TryGetValue("room", out var room)) return null;
        if (!CallRing.ValidRoom(room)) return null;
        var action = values.TryGetValue("action", out var name) ? name switch
        {
            "answer" => CallLaunchAction.Answer,
            "decline" => CallLaunchAction.Decline,
            "open" or "" => CallLaunchAction.Open,
            _ => (CallLaunchAction?)null
        } : CallLaunchAction.Open;
        if (action is null) return null;
        string? ring = null;
        if (values.TryGetValue("ring", out var token) && token.Length > 0)
        {
            if (!CallRing.ValidRingToken(token)) return null;
            ring = token;
        }
        return new CallLaunch(action.Value, room, ring, CallRing.ParseKind(values.GetValueOrDefault("kind")));
    }

    public static string Format(CallLaunchAction action, string room, string? ringToken, string kind)
    {
        var text = "action=" + action.ToString().ToLowerInvariant() + "&room=" + Uri.EscapeDataString(room);
        if (ringToken is not null) text += "&ring=" + Uri.EscapeDataString(ringToken);
        return text + "&kind=" + Uri.EscapeDataString(CallRing.ParseKind(kind));
    }
}

// The incoming-call toast: scenario incomingCall (stays on screen until acted on, bottom right),
// the looping call sound, Answer and Decline. Built as XML here, with every value escaped, so the
// app's toast and the postbox's WNS toast look and behave the same.
public static class CallToast
{
    public const string Group = "calls";
    public const string Sound = "ms-winsoundevent:Notification.Looping.Call";

    // One toast per room; the same tag and group as the postbox's WNS toast (crates/pigeonpost-postbox
    // src/wns.rs tag_for: the room's first 16 characters, WNS's tag limit), so either replaces the
    // other and a ring end removes whichever is showing.
    public const int TagLength = 16;
    public static string Tag(string room) => room.Length <= TagLength ? room : room[..TagLength];

    public static string Title(CallRing ring, string? displayName) =>
        CallRing.Clean(displayName, 60) ?? ring.FromKey ?? "Someone";

    public static string Subtitle(CallRing ring) => "Incoming " + (ring.Video ? "video" : "voice") + " call";

    // Lines as in the WNS toast: the caller's name, "Incoming … call", then the handle (left out when
    // it is already the title). silent: the call page is already playing its ringtone.
    public static string Build(CallRing ring, string? displayName, bool silent = false)
    {
        var title = Title(ring, displayName);
        var handle = ring.FromKey;
        var lines = new List<string> { title, Subtitle(ring) };
        if (handle is not null && !string.Equals(handle, title, StringComparison.Ordinal)) lines.Add(handle);
        var open = CallLaunch.Format(CallLaunchAction.Open, ring.Room, ring.RingToken, ring.Kind);
        var answer = CallLaunch.Format(CallLaunchAction.Answer, ring.Room, ring.RingToken, ring.Kind);
        var decline = CallLaunch.Format(CallLaunchAction.Decline, ring.Room, ring.RingToken, ring.Kind);
        var xml = new StringBuilder();
        xml.Append("<toast scenario=\"incomingCall\" useButtonStyle=\"true\" activationType=\"foreground\" launch=\"").Append(Escape(open)).Append("\">");
        xml.Append("<visual><binding template=\"ToastGeneric\">");
        for (var i = 0; i < lines.Count; i++)
            xml.Append(i == 0 ? "<text hint-maxLines=\"1\">" : "<text>").Append(Escape(lines[i])).Append("</text>");
        xml.Append("</binding></visual>");
        xml.Append(silent ? "<audio silent=\"true\"/>" : "<audio src=\"" + Sound + "\" loop=\"true\"/>");
        xml.Append("<actions>");
        xml.Append("<action content=\"Answer\" activationType=\"foreground\" hint-buttonStyle=\"Success\" arguments=\"").Append(Escape(answer)).Append("\"/>");
        xml.Append("<action content=\"Decline\" activationType=\"foreground\" hint-buttonStyle=\"Critical\" arguments=\"").Append(Escape(decline)).Append("\"/>");
        xml.Append("</actions></toast>");
        return xml.ToString();
    }

    // XML text and attribute escaping (both quote kinds, so the value is safe in either context).
    public static string Escape(string value)
    {
        var builder = new StringBuilder(value.Length + 16);
        foreach (var c in value)
            builder.Append(c switch
            {
                '&' => "&amp;", '<' => "&lt;", '>' => "&gt;", '"' => "&quot;", '\'' => "&apos;",
                _ => c.ToString()
            });
        return builder.ToString();
    }
}

// Which rings are live, whether a toast shows them, and which were already answered or declined
// here (so a late page ring or a WNS duplicate for the same room never rings again).
// Used on the UI thread only.
public sealed class CallRingState
{
    public static readonly TimeSpan MaxRing = TimeSpan.FromSeconds(60);
    private static readonly TimeSpan HandledMemory = TimeSpan.FromMinutes(2);
    private readonly Dictionary<string, Entry> live = new(StringComparer.Ordinal);
    private readonly Dictionary<string, DateTimeOffset> handled = new(StringComparer.Ordinal);

    private sealed class Entry(CallRing ring, DateTimeOffset started)
    {
        public CallRing Ring { get; set; } = ring;
        public DateTimeOffset Started { get; } = started;
        public bool Toast { get; set; }
    }

    // True for a new ring. A ring for a room already live only fills in what was missing (a WNS
    // push brings the ring token, the page brings the code).
    public bool Begin(CallRing ring, DateTimeOffset now)
    {
        Forget(now);
        if (handled.ContainsKey(ring.Room)) return false;
        if (live.TryGetValue(ring.Room, out var entry))
        {
            var known = entry.Ring;
            entry.Ring = known with
            {
                RingToken = known.RingToken ?? ring.RingToken, Code = known.Code ?? ring.Code,
                FromAddress = known.FromAddress ?? ring.FromAddress, FromHandle = known.FromHandle ?? ring.FromHandle, To = known.To ?? ring.To
            };
            return false;
        }
        live[ring.Room] = new Entry(ring, now);
        return true;
    }

    public CallRing? Get(string room) => live.TryGetValue(room, out var entry) ? entry.Ring : null;
    public bool IsLive(string room) => live.ContainsKey(room);
    public bool HasAny => live.Count > 0;
    public bool HasToast(string room) => live.TryGetValue(room, out var entry) && entry.Toast;
    public void MarkToast(string room, bool shown = true) { if (live.TryGetValue(room, out var entry)) entry.Toast = shown; }

    // The rings still sounding without a toast (when the window goes to the background).
    public IReadOnlyList<CallRing> WithoutToast() => live.Values.Where(e => !e.Toast).Select(e => e.Ring).ToArray();

    // The ring stopped (ended, answered or declined); true when it was live.
    public bool End(string room) => live.Remove(room);

    // Answered or declined here: ends it and ignores any later report of the same ring.
    public void Handle(string room, DateTimeOffset now)
    {
        live.Remove(room);
        handled[room] = now;
    }

    public bool WasHandled(string room, DateTimeOffset now) { Forget(now); return handled.ContainsKey(room); }

    // Rings whose end never arrived (the page reloaded, the network dropped): past the calls
    // service's own ring limit they are over, so their toasts must go.
    public IReadOnlyList<string> Expire(DateTimeOffset now)
    {
        var over = live.Where(e => now - e.Value.Started >= MaxRing).Select(e => e.Key).ToArray();
        foreach (var room in over) live.Remove(room);
        return over;
    }

    public void Clear() { live.Clear(); handled.Clear(); }

    private void Forget(DateTimeOffset now)
    {
        foreach (var room in handled.Where(e => now - e.Value >= HandledMemory).Select(e => e.Key).ToArray()) handled.Remove(room);
    }
}

// The calls service's ring-token endpoints (no member token needed): a toast from a push can
// decline its ring without the call page, even when the app was not running.
public static class CallRings
{
    public static readonly Uri ProductionBase = new("https://inbox.pigeonpost.dev/calls/v1/");

    public static async Task<bool> DeclineAsync(HttpClient http, string room, string ringToken, CancellationToken cancellationToken, Uri? callsBase = null)
    {
        if (!CallRing.ValidRoom(room) || !CallRing.ValidRingToken(ringToken)) return false;
        var target = new Uri(callsBase ?? ProductionBase, "rings/" + room + "/decline");
        using var response = await http.PostAsJsonAsync(target, new Dictionary<string, string> { ["ring_token"] = ringToken, ["reason"] = "declined" },
            cancellationToken).ConfigureAwait(false);
        return response.IsSuccessStatusCode;
    }
}

// The postbox's raw WNS notification at the end of a ring (crates/pigeonpost-postbox
// src/call_alerts.rs wns_cancel): {"type":"call_cancel","room":…,"reason":…}. Anything else, or a
// room that is not a room id, is ignored.
public static class CallPush
{
    public const int MaxPayload = 5000; // WNS's own limit.

    public static string? CancelledRoom(byte[]? payload)
    {
        if (payload is null || payload.Length is 0 or > MaxPayload) return null;
        try
        {
            if (JsonNode.Parse(payload) is not JsonObject message) return null;
            if (message["type"] is not JsonValue type || !type.TryGetValue<string>(out var kind) || kind != "call_cancel") return null;
            if (message["room"] is not JsonValue room || !room.TryGetValue<string>(out var value)) return null;
            return CallRing.ValidRoom(value) ? value : null;
        }
        catch (System.Text.Json.JsonException) { return null; }
    }
}

// A WNS channel URI is only ever registered when it really points at Microsoft's push service,
// as the postbox also requires.
public static class WnsChannel
{
    public static bool IsValid(string? uri) =>
        uri is { Length: > 0 and <= 2048 } && Uri.TryCreate(uri, UriKind.Absolute, out var parsed) && parsed.Scheme == Uri.UriSchemeHttps
        && parsed.IsDefaultPort && parsed.IdnHost.EndsWith(".notify.windows.com", StringComparison.OrdinalIgnoreCase)
        && string.IsNullOrEmpty(parsed.UserInfo);

    // Channels last 30 days; renewing on every start and once a day keeps the registration fresh.
    public static readonly TimeSpan RefreshInterval = TimeSpan.FromHours(24);

    // Windows App SDK push (docs/call-alerting.md). The Entra app registration "Pigeonpost Desktop
    // WNS" in the Wodo Network tenant (48477bf1-ca44-4687-a97c-3d39d7ef7572): Microsoft maps the
    // package family name to its application ID, the package's push COM server is registered under
    // that ID, the channel request names the app by its service principal's object ID, and the
    // postbox signs in to WNS as the same application. None of these is a secret.
    public static readonly Guid AzureAppId = new("6b717f08-17c7-4433-8ea8-25f06ed63e60");
    public static readonly Guid AzureObjectId = new("d4a94855-b3f5-41af-8b48-c66d4d7eef73");
    public const string PushServerArguments = "----WindowsAppRuntimePushServer:";
}
