using System.Net;
using System.Text.Json;
using System.Xml.Linq;
using Pigeonpost.Core;

// Phase 1c incoming-call alerting: page ring messages, toast launch arguments, the toast XML and
// the ring bookkeeping (docs/call-alerting.md).
internal static class CallRingTests
{
    private const string Room = "AbCdEfGhIjKlMnOpQrStUv";            // 22 characters, as the calls service mints
    private const string Token = "abcdefghijklmnopqrstuvwxyzABCDEFGHIJ0123456"; // 43 characters, base64url

    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("A page ring is parsed strictly and its caller text is cleaned", () =>
        {
            var message = CallBridge.Parse("{\"type\":\"ring\",\"room\":\"" + Room + "\",\"code\":\"ABC234\",\"kind\":\"video\",\"from\":{\"address\":\"/k/abc\",\"handle\":\"/alp\"},\"to\":\"/bekir\"}")!;
            Equal(message.Type, "ring");
            var ring = message.Ring!;
            Equal(ring.Room, Room); Equal(ring.Kind, "video"); Equal(ring.Code, "ABC234");
            Equal(ring.FromHandle, "/alp"); Equal(ring.FromAddress, "/k/abc"); Equal(ring.To, "/bekir"); Equal(ring.FromKey, "/alp");
            Require(ring.RingToken is null, "A page ring never carries a ring token.");
            Equal(CallBridge.Parse("{\"type\":\"ring\",\"room\":\"" + Room + "\",\"kind\":\"screen\"}")!.Ring!.Kind, "audio");
            var odd = CallBridge.Parse("{\"type\":\"ring\",\"room\":\"" + Room + "\",\"from\":{\"handle\":\"alp\",\"address\":\"/k/a b\"}}")!.Ring!;
            Require(odd.FromHandle is null && odd.FromAddress is null && odd.FromKey is null, "A malformed address was kept.");
            foreach (var bad in new[]
            {
                "{\"type\":\"ring\"}", "{\"type\":\"ring\",\"room\":\"short\"}", "{\"type\":\"ring\",\"room\":\"../../etc/passwd/xx\"}",
                "{\"type\":\"ring\",\"room\":1234567890123}", "{\"type\":\"ring\",\"ringing\":true}", "{\"type\":\"ring_ended\"}",
                "{\"type\":\"ring_ended\",\"room\":\"a b c d e f g\"}", "{\"type\":\"call\",\"state\":\"exploded\"}", "{\"type\":\"muted\",\"muted\":true}"
            })
                Require(CallBridge.Parse(bad) is null, "Accepted an invalid ring message: " + bad);
            Equal(CallBridge.Parse("{\"type\":\"ring_ended\",\"room\":\"" + Room + "\",\"reason\":\"declined\"}")!.Room, Room);
            var call = CallBridge.Parse("{\"type\":\"call\",\"state\":\"active\",\"room\":\"" + Room + "\",\"kind\":\"audio\"}")!;
            Equal(call.State, "active"); Equal(call.Room, Room);
            Require(CallBridge.Parse("{\"type\":\"call\",\"state\":\"ended\",\"room\":\"<x>\"}")!.Room is null, "A malformed call room was kept.");
            return Task.CompletedTask;
        });
        yield return ("Caller text loses control, bidi and zero-width characters and is cut to length", () =>
        {
            Equal(CallRing.Clean("A‮lp​\n\u0007", 60), "Alp");
            Equal(CallRing.Clean(new string('x', 100), 60)!.Length, 60);
            Equal(CallRing.Clean("👩🏽‍💻", 3), "👩"); // Never splits a surrogate pair.
            Equal(CallRing.Clean("a\uD800b", 10), "ab");
            Require(CallRing.Clean(" ‎ ", 10) is null && CallRing.Clean(null, 10) is null, "Blank text survived.");
            return Task.CompletedTask;
        });
        yield return ("Toast launch arguments round-trip and refuse anything that is not one valid ring", () =>
        {
            var answer = CallLaunch.Parse("action=answer&room=" + Room + "&ring=" + Token + "&kind=video")!;
            Equal(answer, new CallLaunch(CallLaunchAction.Answer, Room, Token, "video"));
            Equal(CallLaunch.Parse("action=decline;room=" + Room + ";kind=audio"), new CallLaunch(CallLaunchAction.Decline, Room, null, "audio"));
            Equal(CallLaunch.Parse("room=" + Room)!.Action, CallLaunchAction.Open);
            Equal(CallLaunch.Parse("action%3Danswer&room=" + Room), new CallLaunch(CallLaunchAction.Open, Room, null, "audio")); // "action=answer" key does not exist.
            foreach (var action in new[] { CallLaunchAction.Answer, CallLaunchAction.Decline, CallLaunchAction.Open })
                foreach (var token in new[] { null, Token })
                    Equal(CallLaunch.Parse(CallLaunch.Format(action, Room, token, "video")), new CallLaunch(action, Room, token, "video"));
            foreach (var bad in new[]
            {
                null, "", "mailbox=/k/a&peer=/b&message=m1", "action=answer", "action=hack&room=" + Room, "action=answer&room=" + Room + "&room=" + Room + "x",
                "action=answer&room=" + Room + "&ring=short", "action=answer&room=" + Room + "&ring=" + Token + "%2F", "action=answer&room=bad%20room%20id",
                "action=answer&room=" + Room + "&ring=%ZZ", "action=answer&room=" + Room + "&x=" + new string('a', 2048)
            })
                Require(CallLaunch.Parse(bad) is null, "Accepted invalid launch arguments: " + bad);
            return Task.CompletedTask;
        });
        yield return ("The incoming-call toast is valid XML with every value escaped", () =>
        {
            var ring = new CallRing(Room, "video", "ABC234", "/k/abc", "/alp&<b>\"'", "/bekir", Token);
            var xml = CallToast.Build(ring, "Alp <script>&amp; \"quoted\" 'single'‮");
            var toast = XDocument.Parse(xml).Root!;
            Equal(toast.Name.LocalName, "toast");
            Equal((string?)toast.Attribute("scenario"), "incomingCall");
            Equal(CallLaunch.Parse((string?)toast.Attribute("launch")), new CallLaunch(CallLaunchAction.Open, Room, Token, "video"));
            var texts = toast.Descendants("text").Select(t => t.Value).ToArray();
            Equal(string.Join("|", texts), "Alp <script>&amp; \"quoted\" 'single'|Incoming video call|/alp&<b>\"'");
            var audio = toast.Element("audio")!;
            Equal((string?)audio.Attribute("src"), "ms-winsoundevent:Notification.Looping.Call");
            Equal((string?)audio.Attribute("loop"), "true");
            var actions = toast.Descendants("action").ToArray();
            Equal(string.Join(",", actions.Select(a => (string?)a.Attribute("content"))), "Answer,Decline");
            Equal(CallLaunch.Parse((string?)actions[0].Attribute("arguments")), new CallLaunch(CallLaunchAction.Answer, Room, Token, "video"));
            Equal(CallLaunch.Parse((string?)actions[1].Attribute("arguments")), new CallLaunch(CallLaunchAction.Decline, Room, Token, "video"));
            Require(actions.All(a => (string?)a.Attribute("activationType") == "foreground"), "Buttons must activate the app.");
            // Without a known name the handle is the title and is not repeated; a voice call says so.
            var plain = XDocument.Parse(CallToast.Build(new CallRing(Room, "audio", null, "/k/abc", "/alp", null), null)).Root!;
            Equal(string.Join("|", plain.Descendants("text").Select(t => t.Value)), "/alp|Incoming voice call");
            var quiet = XDocument.Parse(CallToast.Build(ring, null, silent: true)).Root!.Element("audio")!;
            Require((string?)quiet.Attribute("silent") == "true" && quiet.Attribute("src") is null, "A silent toast still rang.");
            Require(plain.Descendants("action").All(a => !((string?)a.Attribute("arguments"))!.Contains("ring=")), "No ring token was invented.");
            var nobody = XDocument.Parse(CallToast.Build(new CallRing(Room, "audio", null, null, null, null), " ")).Root!;
            Equal(nobody.Descendants("text").First().Value, "Someone");
            Equal(CallToast.Tag(Room), "AbCdEfGhIjKlMnOp"); // The postbox's WNS tag for the same room.
            Equal(CallToast.Tag("short12345"), "short12345");
            Equal(CallToast.Group, "calls");
            return Task.CompletedTask;
        });
        yield return ("Ring state rings once per room, merges a push into a page ring, and expires lost rings", () =>
        {
            var state = new CallRingState();
            var now = DateTimeOffset.UnixEpoch.AddDays(1);
            var page = new CallRing(Room, "audio", "ABC234", "/k/abc", "/alp", "/bekir");
            Require(state.Begin(page, now), "A new ring was not reported.");
            Require(!state.Begin(page with { RingToken = Token, Code = null }, now), "The same room rang twice.");
            Equal(state.Get(Room)!.RingToken, Token);
            Equal(state.Get(Room)!.Code, "ABC234");
            Equal(state.WithoutToast().Count, 1);
            state.MarkToast(Room);
            Require(state.HasToast(Room) && state.WithoutToast().Count == 0, "Toast state was not kept.");
            Require(state.End(Room) && !state.End(Room) && !state.IsLive(Room), "A ring ended twice.");
            Require(state.Begin(page, now), "A later ring for the room was ignored.");
            state.Handle(Room, now);
            Require(!state.IsLive(Room) && state.WasHandled(Room, now), "Handling did not end the ring.");
            Require(!state.Begin(page, now.AddSeconds(30)), "An answered ring rang again.");
            Require(state.Begin(page, now.AddMinutes(3)), "A handled room was remembered forever.");
            Equal(state.Expire(now.AddMinutes(3).AddSeconds(59)).Count, 0);
            Equal(string.Join(",", state.Expire(now.AddMinutes(4))), Room);
            Require(!state.IsLive(Room), "An expired ring stayed live.");
            return Task.CompletedTask;
        });
        yield return ("Answer, decline and configure scripts carry JSON-encoded values only", () =>
        {
            Equal(CallBridge.AnswerScript(Room, null), "window.PigeonpostNative && PigeonpostNative.answer && PigeonpostNative.answer(\"" + Room + "\", null)");
            Equal(CallBridge.DeclineScript(Room, Token), "window.PigeonpostNative && PigeonpostNative.decline && PigeonpostNative.decline(\"" + Room + "\", \"" + Token + "\")");
            Require(!CallBridge.AnswerScript("\");alert(1);//", null).Contains("\");alert"), "Room was not JSON-encoded.");
            var configure = CallBridge.ConfigureScript("t", new Mailbox("/k/abc", "/me"), [], nativeRing: true);
            using var options = JsonDocument.Parse(configure[(configure.IndexOf("configure(", StringComparison.Ordinal) + 10)..^1]);
            Require(options.RootElement.GetProperty("nativeRing").GetBoolean(), "nativeRing was not passed.");
            Require(!JsonDocument.Parse(CallBridge.ConfigureScript("t", new Mailbox("/k/abc"), [])[(configure.IndexOf("configure(", StringComparison.Ordinal) + 10)..^1])
                .RootElement.GetProperty("nativeRing").GetBoolean(), "nativeRing defaulted on.");
            Require(CallBridge.RingObserverScript.Contains("type: \"ring_card\""), "The app's ring-card observer must not collide with the page's ring message.");
            return Task.CompletedTask;
        });
        yield return ("A ring-token decline posts only the token to the calls service", async () =>
        {
            HttpRequestMessage? seen = null;
            string? body = null;
            using var http = new HttpClient(new Handler(async (request, ct) =>
            {
                seen = request;
                body = await request.Content!.ReadAsStringAsync(ct);
                return new HttpResponseMessage(HttpStatusCode.OK) { Content = new StringContent("{\"ok\":true}") };
            }));
            Require(await CallRings.DeclineAsync(http, Room, Token, default), "The decline failed.");
            Equal(seen!.Method, HttpMethod.Post);
            Equal(seen.RequestUri!.AbsoluteUri, "https://inbox.pigeonpost.dev/calls/v1/rings/" + Room + "/decline");
            Require(seen.Headers.Authorization is null, "A ring-token decline must not send a member token.");
            using (var json = JsonDocument.Parse(body!))
            {
                Equal(json.RootElement.GetProperty("ring_token").GetString(), Token);
                Equal(json.RootElement.GetProperty("reason").GetString(), "declined");
            }
            seen = null;
            Require(!await CallRings.DeclineAsync(http, "../x/../../y", Token, default) && !await CallRings.DeclineAsync(http, Room, "bad token", default) && seen is null,
                "An invalid room or token reached the network.");
            using var refused = new HttpClient(new Handler((_, _) => Task.FromResult(new HttpResponseMessage(HttpStatusCode.NotFound))));
            Require(!await CallRings.DeclineAsync(refused, Room, Token, default), "A refused decline was reported as done.");
        });
        yield return ("WNS channels register as wns/windows devices and only Microsoft push URIs are accepted", async () =>
        {
            const string channel = "https://db5p.notify.windows.com/?token=AwYAAAB%2bQ%3d%3d";
            Require(WnsChannel.IsValid(channel), "A WNS channel was refused.");
            foreach (var bad in new[] { null, "", "http://db5p.notify.windows.com/?token=a", "https://notify.windows.com.evil.example/?t=a",
                "https://evil.example/db5p.notify.windows.com", "https://user@db5p.notify.windows.com/", "https://db5p.notify.windows.com:8443/",
                "https://db5p.notify.windows.com/?t=" + new string('a', 2048) })
                Require(!WnsChannel.IsValid(bad), "Accepted a channel that is not WNS: " + bad);
            var requests = new List<(HttpMethod Method, string Uri, string? Body)>();
            using var http = new HttpClient(new Handler(async (request, ct) =>
            {
                requests.Add((request.Method, request.RequestUri!.AbsoluteUri, request.Content is null ? null : await request.Content.ReadAsStringAsync(ct)));
                return new HttpResponseMessage(HttpStatusCode.OK) { Content = new StringContent("{\"ok\":true}") };
            }));
            using var postbox = new PostboxClient(http, new FixedTokens());
            await postbox.RegisterDeviceAsync("/k/abc", channel, "wns", "windows", default);
            await postbox.UnregisterDeviceAsync(channel, default);
            Equal(requests[0].Uri, "https://postbox.pigeonpost.dev/v1/devices");
            using (var json = JsonDocument.Parse(requests[0].Body!))
            {
                var root = json.RootElement;
                Equal(root.GetProperty("token").GetString(), channel);
                Equal(root.GetProperty("platform").GetString(), "wns");
                Equal(root.GetProperty("kind").GetString(), "alert");
                Equal(root.GetProperty("os").GetString(), "windows");
                Equal(root.GetProperty("identity").GetString(), "/k/abc");
            }
            Equal(requests[1].Method, HttpMethod.Delete);
            Require(requests[1].Uri.StartsWith("https://postbox.pigeonpost.dev/v1/devices/https%3A%2F%2Fdb5p.notify.windows.com%2F%3Ftoken%3D", StringComparison.Ordinal),
                "The channel was not one escaped path segment: " + requests[1].Uri);
        });
        yield return ("A raw call_cancel push names a valid room and nothing else is accepted", () =>
        {
            static byte[] Bytes(string text) => System.Text.Encoding.UTF8.GetBytes(text);
            // Exactly what the postbox sends (call_alerts.rs wns_cancel).
            Equal(CallPush.CancelledRoom(Bytes("{\"type\":\"call_cancel\",\"room\":\"" + Room + "\",\"reason\":\"answered_elsewhere\"}")), Room);
            Equal(CallPush.CancelledRoom(Bytes("{\"room\":\"" + Room + "\",\"type\":\"call_cancel\"}")), Room);
            foreach (var bad in new[]
            {
                "", "null", "[]", "\"call_cancel\"", "{\"type\":\"call_ring\",\"room\":\"" + Room + "\"}",
                "{\"type\":\"call_cancel\"}", "{\"type\":\"call_cancel\",\"room\":\"short\"}",
                "{\"type\":\"call_cancel\",\"room\":\"../../etc/passwd/xx\"}", "{\"type\":\"call_cancel\",\"room\":1234567890123}",
                "{\"type\":1,\"room\":\"" + Room + "\"}", "{\"type\":\"call_cancel\",\"room\":\"" + Room + "\"", "pigeonpost-wns-probe-raw-push-2026-10-29",
            })
                Require(CallPush.CancelledRoom(Bytes(bad)) is null, "Accepted a raw push that is not a ring end: " + bad);
            Require(CallPush.CancelledRoom(null) is null, "Accepted no payload.");
            var padded = "{\"type\":\"call_cancel\",\"room\":\"" + Room + "\",\"x\":\"" + new string('a', CallPush.MaxPayload) + "\"}";
            Require(CallPush.CancelledRoom(Bytes(padded)) is null, "Accepted a payload over the WNS limit.");
            return Task.CompletedTask;
        });
        yield return ("The package registers the Windows App SDK push server under the Entra application ID", () =>
        {
            var manifest = XDocument.Load(ManifestPath());
            XNamespace com = "http://schemas.microsoft.com/appx/manifest/com/windows10";
            var push = manifest.Descendants(com + "ExeServer").Where(e => (string?)e.Attribute("Arguments") == WnsChannel.PushServerArguments).ToList();
            Equal(push.Count, 1);
            Equal((string?)push[0].Attribute("Executable"), "Pigeonpost.exe");
            var classes = push[0].Elements(com + "Class").Select(c => Guid.Parse((string)c.Attribute("Id")!)).ToList();
            Require(classes.SequenceEqual(new[] { WnsChannel.AzureAppId }), "The push COM class is not the Entra application ID.");
            // The channel request uses the service principal's object ID, a different value.
            Require(WnsChannel.AzureObjectId != WnsChannel.AzureAppId && WnsChannel.AzureObjectId != Guid.Empty, "The channel remote ID is not the object ID.");
            // The app-notification activator stays as it was, so toasts from either source activate the app.
            var notifications = manifest.Descendants(com + "ExeServer").Where(e => (string?)e.Attribute("Arguments") == "----AppNotificationActivated:").ToList();
            Equal(notifications.Count, 1);
            return Task.CompletedTask;
        });
    }

    private static string ManifestPath([System.Runtime.CompilerServices.CallerFilePath] string here = "") =>
        Path.Combine(Path.GetDirectoryName(here)!, "..", "Packaging", "Package.appxmanifest");

    private static void Equal<T>(T actual, T expected)
    {
        if (!EqualityComparer<T>.Default.Equals(actual, expected)) throw new Exception($"Expected {expected}, got {actual}.");
    }
    private static void Require(bool condition, string reason) { if (!condition) throw new Exception(reason); }
    private sealed class FixedTokens : IAccessTokenProvider
    {
        public Task<string> GetTokenAsync(CancellationToken cancellationToken) => Task.FromResult("member");
        public Task<string> RefreshTokenAsync(CancellationToken cancellationToken) => Task.FromResult("member");
    }
}
