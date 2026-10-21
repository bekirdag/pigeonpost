using System.Text.Json;

namespace Pigeonpost.Core;

public enum AppTheme { System, Light, Dark }

// Local, non-secret window preferences. A missing, unreadable or unknown value falls back
// to the default, so a damaged file can never prevent the app from starting.
public sealed class AppPreferences(string path)
{
    private sealed record Stored(string? Theme = null, bool? TrayHintShown = null, string? NotificationPreview = null);

    public static string DefaultPath() => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Pigeonpost", "preferences.json");

    public string FilePath => path;

    public AppTheme LoadTheme() => ParseTheme(Load().Theme);
    public bool LoadTrayHintShown() => Load().TrayHintShown == true;

    public void SaveTheme(AppTheme theme) => Save(Load() with { Theme = theme.ToString().ToLowerInvariant() });
    public void MarkTrayHintShown() => Save(Load() with { TrayHintShown = true });
    public NotificationPreview LoadNotificationPreview() => ParseNotificationPreview(Load().NotificationPreview);
    public void SaveNotificationPreview(NotificationPreview preview) => Save(Load() with { NotificationPreview = preview switch
    {
        Core.NotificationPreview.NameOnly => "name",
        Core.NotificationPreview.Hidden => "hidden",
        _ => "message"
    } });

    private Stored Load()
    {
        try { return JsonSerializer.Deserialize<Stored>(File.ReadAllText(path), PostboxJson.Options) ?? new(); }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException or JsonException) { return new(); }
    }

    private void Save(Stored stored)
    {
        Directory.CreateDirectory(Path.GetDirectoryName(path)!);
        var temporary = path + ".tmp";
        File.WriteAllText(temporary, JsonSerializer.Serialize(stored, PostboxJson.Options));
        File.Move(temporary, path, overwrite: true);
    }

    // Sender and message by default, like other messaging apps; a damaged value never shows more
    // than the user chose, so anything unknown falls back to the most private setting.
    public static NotificationPreview ParseNotificationPreview(string? value) => value?.Trim().ToLowerInvariant() switch
    {
        null or "" or "message" => Core.NotificationPreview.NameAndMessage,
        "name" => Core.NotificationPreview.NameOnly,
        _ => Core.NotificationPreview.Hidden
    };

    public static AppTheme ParseTheme(string? value) => value?.Trim().ToLowerInvariant() switch
    {
        "light" => AppTheme.Light,
        "dark" => AppTheme.Dark,
        _ => AppTheme.System
    };
}

public enum TrayHintAction { None, Show, Log }

// The first close to the notification area explains that Pigeonpost keeps running. It is only
// offered when the icon really exists, shown once ever, and logged (once per run) instead
// when Windows has alerts turned off, so it can still appear later if alerts are enabled.
public static class TrayHint
{
    public static TrayHintAction Decide(bool iconShown, bool alertsEnabled, bool alreadyShown, bool loggedThisRun) =>
        !iconShown || alreadyShown ? TrayHintAction.None
        : alertsEnabled ? TrayHintAction.Show
        : loggedThisRun ? TrayHintAction.None : TrayHintAction.Log;
}
