namespace Pigeonpost.Core;

// What the in-app viewer may show. Anything else (HTML, SVG, scripts, archives, unknown types) is
// only ever saved to disk: the app never renders markup or runs received content.
public enum AttachmentViewKind { None, Image, Video, Audio, Pdf, Text }

public static class AttachmentViewer
{
    // The media type the app may preview or open, from the server's type, or from the file name
    // when the server only says application/octet-stream. Null means save-only.
    public static string? MediaType(string filename, string? mediaType)
    {
        var mime = (mediaType ?? "").Split(';')[0].Trim().ToLowerInvariant();
        if (mime is "" or "application/octet-stream") mime = Path.GetExtension(filename).ToLowerInvariant() switch
        {
            ".png" => "image/png", ".jpg" or ".jpeg" => "image/jpeg", ".gif" => "image/gif",
            ".webp" => "image/webp", ".bmp" => "image/bmp", ".heic" => "image/heic",
            ".mp4" or ".m4v" => "video/mp4", ".mov" => "video/quicktime", ".webm" => "video/webm",
            ".mp3" => "audio/mpeg", ".m4a" => "audio/mp4", ".wav" => "audio/wav",
            ".pdf" => "application/pdf", ".txt" or ".md" or ".csv" or ".json" or ".log" => "text/plain", _ => ""
        };
        return mime switch
        {
            "image/png" or "image/jpeg" or "image/gif" or "image/webp" or "image/bmp" or "image/heic" => mime,
            "video/mp4" or "video/quicktime" or "video/webm" or "audio/mpeg" or "audio/mp4" or "audio/wav" or "audio/x-wav" => mime,
            "application/pdf" or "text/plain" or "text/csv" or "text/markdown" or "application/json" => mime,
            _ => null
        };
    }

    public static AttachmentViewKind Kind(string filename, string? mediaType) => MediaType(filename, mediaType) switch
    {
        null => AttachmentViewKind.None,
        var m when m.StartsWith("image/", StringComparison.Ordinal) => AttachmentViewKind.Image,
        var m when m.StartsWith("video/", StringComparison.Ordinal) => AttachmentViewKind.Video,
        var m when m.StartsWith("audio/", StringComparison.Ordinal) => AttachmentViewKind.Audio,
        "application/pdf" => AttachmentViewKind.Pdf,
        _ => AttachmentViewKind.Text
    };

    // Text files are shown as plain characters, never interpreted; very long files are cut short.
    public const int MaxTextCharacters = 400_000;

    public static string Text(byte[] bytes, out bool truncated)
    {
        var text = new System.Text.UTF8Encoding(false, false).GetString(bytes);
        if (text.Length > 0 && text[0] == '﻿') text = text[1..];
        truncated = text.Length > MaxTextCharacters;
        return truncated ? text[..MaxTextCharacters] : text;
    }

    // A file name that is safe to offer in a Save dialog: no folders, no reserved characters.
    public static string SafeFileName(string filename)
    {
        var name = Path.GetFileName(filename.Replace('\\', '/'));
        foreach (var c in Path.GetInvalidFileNameChars().Concat(['<', '>', ':', '"', '|', '?', '*'])) name = name.Replace(c, '_');
        name = name.Trim().TrimEnd('.');
        return string.IsNullOrWhiteSpace(name) ? "attachment" : name;
    }
}
