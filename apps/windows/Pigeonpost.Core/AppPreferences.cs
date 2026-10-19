using System.Text.Json;

namespace Pigeonpost.Core;

public enum AppTheme { System, Light, Dark }

// Local, non-secret window preferences. A missing, unreadable or unknown value falls back
// to following the system, so a damaged file can never prevent the app from starting.
public sealed class AppPreferences(string path)
{
    private sealed record Stored(string? Theme);

    public static string DefaultPath() => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Pigeonpost", "preferences.json");

    public string FilePath => path;

    public AppTheme LoadTheme()
    {
        try
        {
            var stored = JsonSerializer.Deserialize<Stored>(File.ReadAllText(path), PostboxJson.Options);
            return ParseTheme(stored?.Theme);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException) { return AppTheme.System; }
    }

    public void SaveTheme(AppTheme theme)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        var temporary = path + ".tmp";
        File.WriteAllText(temporary, JsonSerializer.Serialize(new Stored(theme.ToString().ToLowerInvariant()), PostboxJson.Options));
        File.Move(temporary, path, overwrite: true);
    }

    public static AppTheme ParseTheme(string? value) => value?.Trim().ToLowerInvariant() switch
    {
        "light" => AppTheme.Light,
        "dark" => AppTheme.Dark,
        _ => AppTheme.System
    };
}
