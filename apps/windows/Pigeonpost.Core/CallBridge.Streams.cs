using System.Text.Json;
using System.Text.Json.Nodes;
using Pigeonpost.Core.Groups;

namespace Pigeonpost.Core;

// Phase 6: watch a group's live stream, or go live, through the call host's page
// (native-call.js watchStream/goLive, live.js). Each request carries a fresh operation id and its
// answer is accepted only for that operation, from the same page document (MainWindow.Streams.cs).
public static partial class CallBridge
{
    private static CallPageMessage? ParseStream(JsonObject message)
    {
        if (!Guid.TryParseExact(CallRing.Text(message["operation"]), "N", out var operation) || operation == Guid.Empty
            || message["result"] is not JsonObject result) return null;
        var ok = result["ok"] is JsonValue okValue && okValue.TryGetValue<bool>(out var yes) && yes;
        var code = CallRing.Text(result["code"]);
        if (code is not null && (code.Length is < 1 or > 64 || code.Any(c => !char.IsAsciiLetter(c) && !char.IsAsciiDigit(c) && c != '_'))) code = null;
        var stream = CallRing.Text(result["stream"]);
        return new("stream_result", State: ok ? "ok" : "error", Operation: operation,
            ErrorCode: ok ? null : code ?? "unavailable", Stream: GroupStreams.ValidStreamId(stream) ? stream : null);
    }

    public static string WatchStreamScript(string groupId, string groupName, string streamId, Guid operation)
    {
        if (!GroupStreams.ValidGroupId(groupId) || !GroupStreams.ValidStreamId(streamId) || operation == Guid.Empty)
            throw new ArgumentException("A stream request needs a group, a stream and its operation.");
        return StreamRequest(operation, $"native.watchStream({Group(groupId, groupName)}, {JsonSerializer.Serialize(streamId)})", "watchStream");
    }

    public static string GoLiveScript(string groupId, string groupName, string title, bool camera, bool screen, Guid operation)
    {
        if (!GroupStreams.ValidGroupId(groupId) || operation == Guid.Empty || GroupStreams.ValidTitle(title) is not { } clean || !camera && !screen)
            throw new ArgumentException("Going live needs a group, a title, a source and its operation.");
        var options = new JsonObject { ["title"] = clean, ["camera"] = camera, ["screen"] = screen };
        return StreamRequest(operation, $"native.goLive({Group(groupId, groupName)}, {options.ToJsonString()})", "goLive");
    }

    public const string StopStreamScript = "window.PigeonpostNative && PigeonpostNative.stopStream && PigeonpostNative.stopStream()";

    // A stage change for this inbox, read from the postbox, handed to the stream the page shows
    // (the web inbox receives the same `group.stream` event from its event stream; the native page
    // has none). live.js ignores it unless it is for the stream on screen and this mailbox.
    public static string StageScript(string streamId, string address, string state, string? by)
    {
        if (!GroupStreams.ValidStreamId(streamId) || state is not ("invited" or "viewer") || string.IsNullOrEmpty(address) || address.Length > 512)
            throw new ArgumentException("Not a stage change.");
        var data = new JsonObject
        {
            ["stream_id"] = streamId,
            ["stage"] = new JsonObject { ["address"] = address, ["state"] = state, ["by"] = by is { Length: <= 512 } ? by : null },
        };
        return $"window.PigeonpostLive && PigeonpostLive.onGroupStream && PigeonpostLive.onGroupStream({data.ToJsonString()})";
    }

    private static string Group(string id, string name) =>
        new JsonObject { ["id"] = id, ["name"] = GroupStreams.Title(name).Length > 80 ? GroupStreams.Title(name)[..80] : GroupStreams.Title(name) }.ToJsonString();

    private static string StreamRequest(Guid operation, string call, string method) => $$"""
        (() => {
          const send = result => window.chrome.webview.postMessage({ type: "stream_result", operation: {{JsonSerializer.Serialize(operation.ToString("N"))}}, result });
          const native = window.PigeonpostNative;
          if (!native || typeof native.{{method}} !== "function") { send({ ok: false, code: "bridge_unavailable" }); return; }
          Promise.resolve().then(() => {{call}})
            .then(r => send({ ok: Boolean(r && r.ok), code: r && typeof r.code === "string" ? r.code : null, stream: r && typeof r.stream === "string" ? r.stream : null }),
                  () => send({ ok: false, code: "network" }));
        })();
        """;
}
