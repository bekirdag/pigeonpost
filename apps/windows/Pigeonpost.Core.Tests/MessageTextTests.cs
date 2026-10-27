using System.Text;
using System.Text.Json;
using Pigeonpost.Core;

internal static class MessageTextTests
{
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Editing a work envelope changes its displayed task and matching note without changing its verb", () =>
        {
            var original = RequestEnvelope.Work("Original fixture");
            using var changed = JsonDocument.Parse(RequestEnvelope.EditText(original, "Revised fixture"));
            var root = changed.RootElement;
            Require(root.GetProperty("verb").GetString() == "full_access" && root.GetProperty("v").GetInt32() == 1
                && root.GetProperty("args").GetProperty("task").GetString() == "Revised fixture" && root.GetProperty("note").GetString() == "Revised fixture"
                && RequestEnvelope.DisplayText(original) == "Original fixture");
            return Task.CompletedTask;
        });
        yield return ("Editing a structured question retains unrelated arguments, metadata and a different note", () =>
        {
            const string original = "{\"v\":1,\"verb\":\"read_file\",\"args\":{\"question\":\"Original question\",\"path\":\"fixture.txt\",\"nested\":{\"flag\":false}},\"note\":\"Different annotation\",\"context\":{\"source\":\"fixture\"}}";
            using var changed = JsonDocument.Parse(RequestEnvelope.EditText(original, "Revised question"));
            var root = changed.RootElement;
            Require(root.GetProperty("verb").GetString() == "read_file" && root.GetProperty("args").GetProperty("question").GetString() == "Revised question"
                && root.GetProperty("args").GetProperty("path").GetString() == "fixture.txt" && !root.GetProperty("args").GetProperty("nested").GetProperty("flag").GetBoolean()
                && root.GetProperty("note").GetString() == "Different annotation" && root.GetProperty("context").GetProperty("source").GetString() == "fixture");
            return Task.CompletedTask;
        });
        yield return ("An envelope with only a displayed note retains the operation and its arguments", () =>
        {
            using var changed = JsonDocument.Parse(RequestEnvelope.EditText("{\"v\":1,\"verb\":\"run_tests\",\"args\":{\"suite\":\"core\"},\"note\":\"Original note\"}", "Updated note"));
            var root = changed.RootElement;
            Require(root.GetProperty("verb").GetString() == "run_tests" && root.GetProperty("args").GetProperty("suite").GetString() == "core"
                && root.GetProperty("note").GetString() == "Updated note");
            return Task.CompletedTask;
        });
        yield return ("Plain or unknown-format messages remain plain edits rather than newly privileged work requests", () =>
        {
            foreach (var body in new[] { "Plain fixture", "{broken fixture", "{\"value\":\"fixture\"}", "{\"v\":2,\"verb\":\"future\"}" })
                Require(RequestEnvelope.EditText(body, "Updated plain text") == "Updated plain text");
            return Task.CompletedTask;
        });
        yield return ("Ambiguous structured fields are refused rather than silently changing the request operation", () =>
        {
            foreach (var body in new[] { "{\"v\":1,\"verb\":\"read_file\",\"verb\":\"run_shell\",\"note\":\"fixture\"}",
                "{\"v\":1,\"verb\":\"read_file\",\"args\":{\"task\":\"first\",\"task\":\"second\"}}" })
            {
                var refused = false; try { RequestEnvelope.EditText(body, "Updated fixture"); } catch (ArgumentException) { refused = true; }
                Require(refused);
            }
            return Task.CompletedTask;
        });
        yield return ("Reply quotes use the first trimmed line and preserve 200 whole Unicode characters", () =>
        {
            Require(RequestEnvelope.ReplyQuote(" \tFirst line\r\nSecond line ") == "First line");
            var quote = RequestEnvelope.ReplyQuote(string.Concat(Enumerable.Repeat("😀", 220)) + "\nIgnored line");
            Require(quote.EnumerateRunes().Count() == 200 && quote.Length == 400 && quote == string.Concat(Enumerable.Repeat("😀", 200)));
            Require(RequestEnvelope.ReplyQuote("  \r\n ") == "");
            return Task.CompletedTask;
        });
    }
    private static void Require(bool value) { if (!value) throw new Exception("Message text preservation assertion failed."); }
}
