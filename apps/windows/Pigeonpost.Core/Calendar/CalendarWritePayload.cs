using System.Text;
using System.Text.Json;

namespace Pigeonpost.Core.Calendar;

// Freeze the exact body once, before protection or token acquisition. Never log its contents.
public sealed class CalendarWritePayload
{
    internal string WireJson { get; }
    public int ByteCount => Encoding.UTF8.GetByteCount(WireJson);
    public CalendarWritePayload(JsonElement fields)
    {
        if (fields.ValueKind != JsonValueKind.Object) throw new ArgumentException("A calendar request must contain fields.");
        using var stream = new MemoryStream();
        using (var writer = new Utf8JsonWriter(stream)) Canonical(writer, fields, 0);
        if (stream.Length > 128 * 1024) throw new ArgumentException("This calendar request is too large.");
        WireJson = Encoding.UTF8.GetString(stream.ToArray());
    }
    public static CalendarWritePayload From(object fields) => new(JsonSerializer.SerializeToElement(fields, PostboxJson.Options));
    public static CalendarWritePayload Restore(string encoded)
    {
        if (Encoding.UTF8.GetByteCount(encoded) > 128 * 1024) throw new ArgumentException("This saved calendar request is too large.");
        try
        {
            using var document = JsonDocument.Parse(encoded, new JsonDocumentOptions { MaxDepth = 32 });
            var payload = new CalendarWritePayload(document.RootElement);
            if (payload.WireJson != encoded) throw new ArgumentException("This saved calendar request cannot be retried safely.");
            return payload;
        }
        catch (JsonException) { throw new ArgumentException("This saved calendar request is unreadable."); }
    }
    public JsonElement Fields()
    { using var document = JsonDocument.Parse(WireJson); return document.RootElement.Clone(); }
    public string RequestKey()
    {
        var fields = Fields();
        if (!fields.TryGetProperty("request_key", out var key) || key.ValueKind != JsonValueKind.String
            || key.GetString() is not { } value || value.Length is < 1 or > 64 || value.Any(c => !char.IsAsciiLetterOrDigit(c) && c is not '_' and not '-'))
            throw new ArgumentException("This calendar request has no valid request key.");
        return value;
    }
    private static void Canonical(Utf8JsonWriter writer, JsonElement value, int depth)
    {
        if (depth > 31) throw new ArgumentException("This calendar request is too deeply nested.");
        switch (value.ValueKind)
        {
            case JsonValueKind.Object:
                writer.WriteStartObject();
                var names = new HashSet<string>(StringComparer.Ordinal);
                foreach (var field in value.EnumerateObject().OrderBy(p => p.Name, StringComparer.Ordinal))
                {
                    if (!names.Add(field.Name)) throw new ArgumentException("This calendar request has ambiguous fields.");
                    writer.WritePropertyName(field.Name); Canonical(writer, field.Value, depth + 1);
                }
                writer.WriteEndObject(); break;
            case JsonValueKind.Array:
                writer.WriteStartArray(); foreach (var item in value.EnumerateArray()) Canonical(writer, item, depth + 1); writer.WriteEndArray(); break;
            default: value.WriteTo(writer); break;
        }
    }
    public override string ToString() => $"CalendarWritePayload({ByteCount} bytes)";
}
