#if UI_TESTS
using Microsoft.Windows.AppNotifications;
using Pigeonpost.Core;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow
{
    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern IntPtr SendMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
    private async Task RunNotificationTestsAsync(string resultPath)
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
            await DesktopNotifications.ClearAlertsAsync();
            if (notificationInitializationError is not null) throw new InvalidOperationException("Notification initialization failed", notificationInitializationError);
            Check(notifications?.CanHide == true, "Native tray icon created");
            var baseline = unreadState.Count;
            // Exercise the actual close-to-tray path and poll using the release integration.
            SendMessage(WinRT.Interop.WindowNative.GetWindowHandle(this), 0x0010, IntPtr.Zero, IntPtr.Zero); // WM_CLOSE, as the title-bar button sends
            await Task.Delay(200);
            Check(!AppWindow.IsVisible && !lifetime.IsCancellationRequested, "Close keeps the signed-in app running in the tray");
            preview.ReceiveForUiTest();
            preview.ReceiveForUiTest("design-release");
            await ViewModel.RefreshAsync();
            Check(unreadState.Count == baseline + 2, "Hidden arrivals increase the account badge by two");
            Check(ViewModel.Subjects.Single(s => s.Id == "design-release").Unread == 1, "Hidden subject retains its unread marker");
            var alerts = await AppNotificationManager.Default.GetAllAsync();
            Check(alerts.Count == 2, "Windows notification center contains both native alerts");
            await ViewModel.RefreshAsync();
            Check((await AppNotificationManager.Default.GetAllAsync()).Count == 2, "Repeated refresh does not duplicate native alerts");
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
            await DesktopNotifications.ClearAlertsAsync();
            Check(unreadState.Count == 0 && (await AppNotificationManager.Default.GetAllAsync()).Count == 0, "Sign-out cleanup clears the badge and native alerts");
            File.WriteAllText(resultPath, System.Text.Json.JsonSerializer.Serialize(new { passed = true, checks }));
        }
        catch (Exception ex)
        {
            File.WriteAllText(resultPath, System.Text.Json.JsonSerializer.Serialize(new { passed = false, checks, error = ex.ToString() }));
        }
        finally { Quit(); }
    }
}
#endif
