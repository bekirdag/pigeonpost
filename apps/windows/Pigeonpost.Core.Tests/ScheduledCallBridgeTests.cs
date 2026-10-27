using System.Text.Json;
using Pigeonpost.Core;
using Pigeonpost.Core.Calendar;

internal static partial class CalendarStoreTests
{
    public static IEnumerable<(string Name, Func<Task> Run)> MeetingBridge()
    {
        yield return ("Scheduled bridge results carry only a valid operation, known state and bounded error code", () =>
        {
            var id = Guid.NewGuid();
            var waiting = CallBridge.Parse(JsonSerializer.Serialize(new { type = "scheduled_result", operation = id.ToString("N"), result = new { state = "waiting", role = "host", ticket = "ignore", message = "ignore" } }));
            Require(waiting is { Type: "scheduled_result", State: "waiting", ErrorCode: null, Text: null } && waiting.Operation == id);
            foreach (var state in new[] { "joined", "cancelled", "choose_occurrence" }) Require(CallBridge.Parse(JsonSerializer.Serialize(new { type = "scheduled_result", operation = id.ToString("N"), result = new { state } }))?.State == state);
            var error = CallBridge.Parse(JsonSerializer.Serialize(new { type = "scheduled_result", operation = id.ToString("N"), result = new { state = "error", code = "window_closed", message = "not UI text" } }));
            Require(error?.ErrorCode == "window_closed" && error.Text is null); return Task.CompletedTask;
        });
        yield return ("Malformed or duplicate scheduled bridge payloads cannot produce a usable result or crash parsing", () =>
        {
            var id = Guid.NewGuid().ToString("N");
            foreach (var value in new[] { "", "null", "[]", "{", "{\"type\":\"scheduled_result\"}",
                JsonSerializer.Serialize(new { type = "scheduled_result", operation = Guid.Empty.ToString("N"), result = new { state = "joined" } }),
                JsonSerializer.Serialize(new { type = "scheduled_result", operation = id, result = new { state = "host" } }),
                "{\"type\":\"ready\",\"type\":\"scheduled_result\"}",
                "{\"type\":\"scheduled_result\",\"operation\":\"" + id + "\",\"result\":{\"state\":\"waiting\",\"state\":\"joined\"}}", new string('x', 16_385) }) Require(CallBridge.Parse(value) is null);
            var error = CallBridge.Parse(JsonSerializer.Serialize(new { type = "scheduled_result", operation = id, result = new { state = "error", code = "https://fixture.invalid/#private" } }));
            Require(error?.ErrorCode == "unavailable"); return Task.CompletedTask;
        });
        yield return ("Scheduled call scripts serialize only reviewed member inputs and cannot inject a title into JavaScript", async () =>
        {
            using var f = new Fixture(); f.Source.Detail = (_, key) => Task.FromResult(Meeting(key) with { Event = Meeting(key).Event with { Title = "\");globalThis.injected=true;//\n😀" } });
            await f.Model.ActivateAsync(A, Features); using var action = f.Model.CaptureView()!.BeginAction()!;
            using var join = (await f.Model.PrepareScheduledJoinAsync(action, Key("a"), "ABCDEFGHJK", false, f.Model.State.ContextId))!;
            var id = Guid.NewGuid(); var script = CallBridge.ScheduledJoinScript(join, id);
            Require(script.Contains(id.ToString("N"), StringComparison.Ordinal) && script.Contains("\\u0022", StringComparison.Ordinal) && !script.Contains("\");globalThis.injected", StringComparison.Ordinal)
                && !script.Contains("ticket", StringComparison.Ordinal) && !script.Contains("role", StringComparison.Ordinal) && script.Contains("\"kind\":\"audio\"", StringComparison.Ordinal));
            join.Dispose(); Require(Retired(() => CallBridge.ScheduledJoinScript(join, id)));
        });
    }
}
