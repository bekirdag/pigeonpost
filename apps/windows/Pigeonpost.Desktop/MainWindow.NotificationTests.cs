#if UI_TESTS
using Microsoft.Windows.AppNotifications;
using Pigeonpost.Core;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow
{
    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern IntPtr SendMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
    private async Task RunNotificationTestsAsync(string resultPath, PackagedNotificationTest? packaged = null)
    {
        var checks = new List<string>();
        void Check(bool condition, string message)
        {
            if (!condition) throw new InvalidOperationException(message);
            checks.Add(message);
        }
        try
        {
            refreshTimer?.Stop();
            DesktopLog.Write("Notification checks: clearing alerts");
            if (packaged is null) await DesktopNotifications.ClearAlertsAsync(); // Unsupported in the elevated shell session.
            if (notificationInitializationError is not null) throw new InvalidOperationException("Notification initialization failed", notificationInitializationError);
            Check(notifications is not null, "Native notification services created");
            var shell = notifications!.ShellIconState();
            // Only the taskbar owner's session can hold the icon: a standard user beside the runner's
            // elevated Explorer is blocked by Windows (UIPI), which never happens on a real PC.
            var trayExpected = packaged?.Mode == "shell";
            if (trayExpected) Check(notifications.CanHide, "Shell added the notification-area icon");
            else checks.Add($"Notification-area icon {(notifications.CanHide ? "added" : "not available to this session")} (0x{shell:X8})");
            var setting = AppNotificationManager.Default.Setting;
            var identity = DesktopLog.PackageIdentity();
            var alertsExpected = packaged is null || packaged.Mode == "alerts";
            if (packaged is not null)
            {
                Check(identity != "unpackaged", "Runs with the installed package identity");
                if (packaged.Mode == "shell") Check(shell == 0, $"Windows shell holds the notification-area icon (0x{shell:X8})");
                if (alertsExpected) Check(setting == AppNotificationSetting.Enabled, $"Windows allows Pigeonpost alerts ({setting})");
            }
            var baseline = unreadState.Count;
            // Exercise the actual close-to-tray path and poll using the release integration.
            if (notifications.CanHide)
            {
                SendMessage(WinRT.Interop.WindowNative.GetWindowHandle(this), 0x0010, IntPtr.Zero, IntPtr.Zero); // WM_CLOSE, as the title-bar button sends
                await Task.Delay(200);
                Check(!AppWindow.IsVisible && !lifetime.IsCancellationRequested, "Close keeps the signed-in app running in the tray");
            }
            else { AppWindow.Hide(); await Task.Delay(200); checks.Add("Window hidden directly (closing without an icon exits, by design)"); }
            preview.ReceiveForUiTest();
            preview.ReceiveForUiTest("design-release");
            await ViewModel.RefreshAsync();
            Check(unreadState.Count == baseline + 2, "Hidden arrivals increase the account badge by two");
            Check(notifications!.UnreadCount == unreadState.Count, "Tray icon reflects the account unread count");
            Check(ViewModel.Subjects.Single(s => s.Id == "design-release").Unread == 1, "Hidden subject retains its unread marker");
            DesktopLog.Write("Notification checks: arrivals observed");
            var alerts = alertsExpected ? await AppNotificationManager.Default.GetAllAsync() : new List<AppNotification>();
            if (alertsExpected) Check(alerts.Count == 2, "Windows notification center contains both native alerts");
            else checks.Add($"Alerts not asserted in this session ({setting}, {alerts.Count} in history)");
            if (packaged is not null)
            {
                // Let the harness photograph the banner and notification area while they are shown.
                File.WriteAllText(Path.Combine(packaged.Signals, "alerts.ready"), "");
                for (var n = 0; n < 150 && !File.Exists(Path.Combine(packaged.Signals, "alerts.captured")); n++) await Task.Delay(100);
            }
            await ViewModel.RefreshAsync();
            if (alertsExpected) Check((await AppNotificationManager.Default.GetAllAsync()).Count == 2, "Repeated refresh does not duplicate native alerts");
            var snapshot = await preview.LoadAsync(ViewModel.SelectedMailbox!.Address, lifetime.Token);
            var message = snapshot.Messages.Last(m => m.ThreadId == "design-release");
            await OpenNotificationAsync(new(ViewModel.SelectedMailbox.Address, message.PeerKey, message.ThreadId, message.MessageId));
            Check(AppWindow.IsVisible && ViewModel.SelectedSubject?.Id == "design-release", "Notification target restores the window and opens the original subject");
            // Foreground activation is controlled by Windows. Use the same visibility predicate,
            // and assert that acknowledging the opened subject never clears the other thread.
            await ViewModel.AcknowledgeSelectedAsync(() => true);
            Check(ViewModel.SelectedSubject!.Unread == 0 && ViewModel.Subjects.Single(s => s.Id == "design-general").Unread > 0,
                "Reading notification target preserves unread mail in the other subject");
            unreadState.Clear(); notifications!.Update(0);
            if (alertsExpected) await DesktopNotifications.ClearAlertsAsync();
            Check(unreadState.Count == 0 && (!alertsExpected || (await AppNotificationManager.Default.GetAllAsync()).Count == 0), "Sign-out cleanup clears the badge and native alerts");
            File.WriteAllText(resultPath, System.Text.Json.JsonSerializer.Serialize(new { passed = true, checks, identity, shell = $"0x{shell:X8}", setting = setting.ToString(), taskbarWarning = notifications.TaskbarWarning }));
        }
        catch (Exception ex)
        {
            DesktopLog.Error("Notification integration failed", ex);
            File.WriteAllText(resultPath, System.Text.Json.JsonSerializer.Serialize(new { passed = false, checks, identity = DesktopLog.PackageIdentity(), error = ex.ToString() }));
        }
        finally { Quit(); }
    }
}

// Mode "shell": the taskbar owner's (elevated) runner session, which proves identity and the icon.
// Windows does not support alerts for elevated apps; the standard-user run proves alert delivery.
internal sealed record PackagedNotificationTest(string Mode, string Result, string Signals)
{
    private const string Folder = @"C:\PigeonpostUiTest";
    public static PackagedNotificationTest? Load()
    {
        var request = Path.Combine(Folder, "packaged-notifications.request");
        if (!File.Exists(request)) return null;
        var mode = File.ReadAllText(request).Trim();
        return new(mode, Path.Combine(Folder, $"packaged-notifications-{mode}.json"), Folder);
    }
}
#endif
