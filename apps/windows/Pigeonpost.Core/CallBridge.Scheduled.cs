using System.Text.Json;
using System.Text.Json.Nodes;
using Pigeonpost.Core.Calendar;

namespace Pigeonpost.Core;

public static partial class CallBridge
{
    private static CallPageMessage? ParseScheduled(JsonObject message)
    {
        if (!Guid.TryParseExact(CallRing.Text(message["operation"]), "N", out var operation) || operation == Guid.Empty
            || message["result"] is not JsonObject result) return null;
        var state = CallRing.Text(result["state"]);
        if (state is not ("joined" or "waiting" or "cancelled" or "choose_occurrence" or "error")) return null;
        var code = CallRing.Text(result["code"]);
        if (state == "error" && (code is null || code.Length is < 1 or > 64 || code.Any(c => !char.IsAsciiLetterLower(c) && !char.IsAsciiDigit(c) && c != '_'))) code = "unavailable";
        return new("scheduled_result", State: state, Operation: operation, ErrorCode: state == "error" ? code : null);
    }
    public static string ScheduledJoinScript(CalendarScheduledJoin request, Guid operation)
    {
        request.RequirePreparedCurrent(); if (operation == Guid.Empty) throw new ArgumentException("A scheduled call needs its original operation.");
        var options = new JsonObject { ["as"] = request.Mailbox, ["occurrenceId"] = request.OccurrenceId, ["title"] = request.Title, ["kind"] = request.Video ? "video" : "audio" };
        return $$"""
            (() => {
              const send = result => window.chrome.webview.postMessage({ type: "scheduled_result", operation: {{JsonSerializer.Serialize(operation.ToString("N"))}}, result });
              const native = window.PigeonpostNative;
              if (!native || typeof native.joinScheduled !== "function") { send({ state: "error", code: "bridge_unavailable" }); return; }
              Promise.resolve().then(() => native.joinScheduled({{JsonSerializer.Serialize(request.Code)}}, {{options.ToJsonString()}}))
                .then(send, () => send({ state: "error", code: "network" }));
            })();
            """;
    }
}
