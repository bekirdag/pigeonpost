namespace Pigeonpost.Desktop;

// Local diagnostics for notification, tray and connection problems on a user's PC. Entries
// describe app state and errors only: never message text, addresses or tokens. Packaged
// installs keep this under %LOCALAPPDATA%\Packages\<package>\LocalCache\Local\Pigeonpost\logs.
internal static class DesktopLog
{
    private static readonly object Gate = new();
    private const long MaxBytes = 1_000_000;

    public static string DirectoryPath { get; } = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Pigeonpost", "logs");

    public static void Write(string message)
    {
        try
        {
            lock (Gate)
            {
                Directory.CreateDirectory(DirectoryPath);
                var file = Path.Combine(DirectoryPath, "desktop.log");
                if (File.Exists(file) && new FileInfo(file).Length > MaxBytes)
                    File.Move(file, Path.Combine(DirectoryPath, "desktop.1.log"), overwrite: true);
                File.AppendAllText(file, $"{DateTimeOffset.Now:O} [{Environment.ProcessId}] {message}{Environment.NewLine}");
            }
        }
        catch (Exception) { /* Diagnostics must never affect the app. */ }
    }

    public static void Error(string context, Exception ex) =>
        Write($"{context}: {ex.GetType().Name} 0x{ex.HResult:X8} {ex.Message.ReplaceLineEndings(" ")}");

    public static string PackageIdentity()
    {
        try { return Windows.ApplicationModel.Package.Current.Id.FullName; }
        catch (Exception) { return "unpackaged"; }
    }
}
