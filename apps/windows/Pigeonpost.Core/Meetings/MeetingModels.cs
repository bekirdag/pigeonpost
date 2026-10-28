using System.Globalization;
using System.Text;
using System.Text.Json;

namespace Pigeonpost.Core.Meetings;

// Phase 7 meeting notes (docs/meeting-summaries.md §2): the postbox's records. A brief is stored
// only as ciphertext plus its content key wrapped for each of the account's notes keys.

public sealed record MeetingSummary
{
    public required string Id { get; init; }
    public string? Kind { get; init; }
    public string? State { get; init; } // active | processing | ready | failed
    public long? CreatedAt { get; init; }
    public long? EndedAt { get; init; }
    public long? ReadyAt { get; init; }
    public long? ExpiresAt { get; init; }
    public string? Host { get; init; }
    public string? As { get; init; }
    public string? Failure { get; init; }
}

public sealed record MeetingWrap
{
    public required string KeyId { get; init; }
    public required string Epk { get; init; }
    public required string Iv { get; init; }
    public required string Wrapped { get; init; }
}

public sealed record MeetingRecord
{
    public required string Id { get; init; }
    public string? Kind { get; init; }
    public string? State { get; init; }
    public long? CreatedAt { get; init; }
    public long? EndedAt { get; init; }
    public long? ReadyAt { get; init; }
    public long? ExpiresAt { get; init; }
    public string? Host { get; init; }
    public string? Failure { get; init; }
    public string? Alg { get; init; }
    public string? Iv { get; init; }
    public string? Ciphertext { get; init; }
    public IReadOnlyList<MeetingWrap>? Keys { get; init; }
}

public sealed record MeetingActionItem(string Task, string? Owner, string? Due)
{
    public string Text => string.Join(" ", new[] { Owner is null ? null : Owner + ":", Task, Due is null ? null : $"(due {Due})" }.Where(p => p is not null));
}

public sealed record MeetingTranscriptLine(string At, string Speaker, string Text);

// The decrypted brief (meeting-intel's JSON). Everything is plain text; nothing is ever rendered
// as markup. Lists and strings are bounded so a damaged brief cannot exhaust the window.
public sealed record MeetingBrief(
    string? Title, string? Summary, IReadOnlyList<string> Decisions, IReadOnlyList<MeetingActionItem> ActionItems,
    IReadOnlyList<string> OpenQuestions, IReadOnlyList<MeetingTranscriptLine> Transcript, IReadOnlyList<string> Participants,
    DateTimeOffset? StartedAt, long? DurationSeconds, bool Draft, string? Stt, string? Llm)
{
    private const int MaxItems = 2_000, MaxText = 20_000;

    public static MeetingBrief Parse(ReadOnlySpan<byte> json, string meetingId)
    {
        using var document = JsonDocument.Parse(json.ToArray(), new JsonDocumentOptions { MaxDepth = 16 });
        var root = document.RootElement;
        if (root.ValueKind != JsonValueKind.Object) throw new MeetingNotesException("bad_brief");
        if (root.TryGetProperty("v", out var v) && (v.ValueKind != JsonValueKind.Number || !v.TryGetInt32(out var version) || version != 1))
            throw new MeetingNotesException("bad_brief");
        // The content is already bound to the meeting by its additional data; a brief that names a
        // different meeting is refused as well.
        if (root.TryGetProperty("meeting_id", out var mid) && (mid.ValueKind != JsonValueKind.String || mid.GetString() != meetingId))
            throw new MeetingNotesException("bad_brief");
        DateTimeOffset? started = Text(root, "started_at") is { } s && DateTimeOffset.TryParse(s, CultureInfo.InvariantCulture, DateTimeStyles.AssumeUniversal, out var at) ? at : null;
        long? duration = root.TryGetProperty("duration_s", out var d) && d.ValueKind == JsonValueKind.Number && d.TryGetInt64(out var secs) && secs is >= 0 and <= 86_400 ? secs : null;
        var generated = root.TryGetProperty("generated_by", out var g) && g.ValueKind == JsonValueKind.Object ? g : default;
        return new(Text(root, "title"), Text(root, "summary"), Strings(root, "decisions"),
            Items(root, "action_items", e => Text(e, "task") is { } task ? new MeetingActionItem(task, Text(e, "owner"), Text(e, "due")) : null),
            Strings(root, "open_questions"),
            Items(root, "transcript", e => Text(e, "text") is { } text ? new MeetingTranscriptLine(Text(e, "at") ?? "", Text(e, "speaker") ?? "Someone", text) : null),
            Strings(root, "participants"), started, duration,
            root.TryGetProperty("draft", out var draft) && draft.ValueKind == JsonValueKind.True,
            generated.ValueKind == JsonValueKind.Object ? Text(generated, "stt") : null,
            generated.ValueKind == JsonValueKind.Object ? Text(generated, "llm") : null);
    }

    private static string? Text(JsonElement parent, string name) =>
        parent.ValueKind == JsonValueKind.Object && parent.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String ? Clean(value.GetString()) : null;

    // Plain text: control characters other than line breaks become spaces; bounded length.
    internal static string? Clean(string? text)
    {
        if (string.IsNullOrWhiteSpace(text)) return null;
        var builder = new StringBuilder(Math.Min(text.Length, MaxText));
        foreach (var c in text)
        {
            if (builder.Length >= MaxText) break;
            builder.Append(c == '\n' ? c : char.IsControl(c) || c is '‪' or '‫' or '‭' or '‮' or '⁦' or '⁧' or '⁨' ? ' ' : c);
        }
        var result = builder.ToString().Trim();
        return result.Length == 0 ? null : result;
    }

    private static IReadOnlyList<string> Strings(JsonElement root, string name) =>
        Items(root, name, e => e.ValueKind == JsonValueKind.String ? Clean(e.GetString()) : null);

    private static IReadOnlyList<T> Items<T>(JsonElement root, string name, Func<JsonElement, T?> read) where T : class
    {
        if (!root.TryGetProperty(name, out var list) || list.ValueKind != JsonValueKind.Array) return [];
        var result = new List<T>();
        foreach (var e in list.EnumerateArray())
        {
            if (result.Count >= MaxItems) break;
            if (read(e) is { } item) result.Add(item);
        }
        return result;
    }

    public string Heading => Title ?? "Meeting notes";

    public string MetaLine => string.Join(" · ", new[]
    {
        StartedAt?.ToLocalTime().ToString("g", CultureInfo.CurrentCulture),
        DurationSeconds is { } s ? Math.Max(1, (long)Math.Round(s / 60.0, MidpointRounding.AwayFromZero)) + " min" : null,
        Participants.Count > 0 ? string.Join(", ", Participants) : null,
    }.Where(p => p is not null));

    public string GeneratedLine => "Transcribed and summarized automatically on Pigeonpost's server"
        + (Stt is not null ? $" ({Stt}{(Llm is not null ? ", " + Llm : "")})" : "")
        + ". Speaker names come from each person's own microphone; a microphone can pick up other voices.";

    // "Copy as text", as the web reader writes it.
    public string AsText()
    {
        var lines = new List<string> { Heading, "" };
        if (Summary is not null) lines.AddRange([Summary, ""]);
        void List<T>(string title, IReadOnlyList<T> items, Func<T, string> line)
        {
            if (items.Count == 0) return;
            lines.Add(title + ":");
            lines.AddRange(items.Select(i => "- " + line(i)));
            lines.Add("");
        }
        List("Decisions", Decisions, x => x);
        List("Action items", ActionItems, a => a.Text);
        List("Open questions", OpenQuestions, x => x);
        List("Transcript", Transcript, t => $"[{t.At}] {t.Speaker}: {t.Text}");
        return string.Join("\n", lines);
    }
}

public sealed class MeetingNotesException(string code) : Exception(code switch
{
    "no_key_for_device" => "These notes were encrypted for your other devices. Open them on a device that was in the call, or one that opened Meeting notes before the call ended.",
    "not_ready" => "The notes are still being prepared. Check back in a few minutes.",
    _ => "This device couldn't open these notes.",
})
{
    public string Code { get; } = code;
}
