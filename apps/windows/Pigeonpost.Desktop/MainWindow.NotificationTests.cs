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
        string? toastPayload = null;
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
            Check(notifications is not null, "Native notification services created");
            var shell = notifications!.ShellIconState();
            // This standard user sits beside the runner's elevated Explorer, which Windows (UIPI) does not
            // let it reach: the icon is proven by the installed package in the taskbar owner's session
            // (smoke-test.ps1 -PackageFamilyName). Alerts are proven here, where Windows supports them.
            checks.Add($"Notification-area icon {(notifications.CanHide ? "added" : "not available to this session")} (0x{shell:X8})");
            var setting = AppNotificationManager.Default.Setting;
            var identity = DesktopLog.PackageIdentity();
            Check(setting == AppNotificationSetting.Enabled, $"Windows allows Pigeonpost alerts ({setting})");
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
            var alerts = await AppNotificationManager.Default.GetAllAsync();
            Check(alerts.Count == 2, "Windows notification center contains both native alerts");
            var mailboxAddress = ViewModel.SelectedMailbox!.Address;
            var designGroup = MessageAlerts.GroupKey(mailboxAddress, "/preview/design");
            foreach (var raw in alerts)
            {
                // Compare attribute values whichever quote style Windows serialises them with.
                var alert = new { raw.Group, Payload = raw.Payload.Replace('\'', '"') };
                checks.Add("Toast payload: " + raw.Payload);
                // The toast Windows holds: sender's name (the contact alias), the message, an inline
                // reply box, Mark as read, the sender's face, filed under the conversation.
                Check(alert.Group == designGroup, "Toast is grouped under its conversation");
                Check(alert.Payload.Contains(">Design<", StringComparison.Ordinal), "Toast names the sender: " + alert.Payload);
                Check(alert.Payload.Contains("New message during background refresh", StringComparison.Ordinal), "Toast previews the message");
                Check(alert.Payload.Contains("<input ", StringComparison.Ordinal) && alert.Payload.Contains("id=\"reply\"", StringComparison.Ordinal), "Toast has an inline Reply box: " + alert.Payload);
                Check(alert.Payload.Contains("content=\"Reply\"", StringComparison.Ordinal) && alert.Payload.Contains("hint-inputId=\"reply\"", StringComparison.Ordinal), "Toast has a Reply button bound to the box: " + alert.Payload);
                Check(alert.Payload.Contains("content=\"Mark as read\"", StringComparison.Ordinal), "Toast has Mark as read: " + alert.Payload);
                Check(alert.Payload.Contains("appLogoOverride", StringComparison.Ordinal) && alert.Payload.Contains("hint-crop=\"circle\"", StringComparison.Ordinal), "Toast shows the sender's face: " + alert.Payload);
            }
            toastPayload = alerts[0].Payload;
            checks.Add($"Toast ids {string.Join(", ", alerts.Select(a => a.Id))} raised through AppNotificationManager while the window was hidden");
            if (Environment.GetEnvironmentVariable("PIGEONPOST_NOTIFICATION_TEST_SCREENSHOT") is { Length: > 0 } screenshot)
            {
                await Task.Delay(1500); // The banner slides in at the bottom right.
                CaptureScreen(screenshot);
                checks.Add("Screen captured while the toast was showing: " + Path.GetFileName(screenshot));
            }

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
            // Toast actions run with the window hidden: the preview setting, Mark as read and Reply.
            AppWindow.Hide(); await Task.Delay(200);
            var previous = preferences.LoadNotificationPreview();
            preferences.SaveNotificationPreview(NotificationPreview.Hidden);
            try
            {
                preview.ReceiveForUiTest();
                await ViewModel.RefreshAsync();
                var privateAlert = (await AppNotificationManager.Default.GetAllAsync()).OrderBy(a => a.Id).Last();
                Check(privateAlert.Payload.Contains(MessageAlerts.GenericText, StringComparison.Ordinal)
                    && !privateAlert.Payload.Contains("background refresh", StringComparison.Ordinal) && !privateAlert.Payload.Contains(">Design<", StringComparison.Ordinal),
                    "With previews off the toast names neither the sender nor the message");
            }
            finally { preferences.SaveNotificationPreview(previous); }
            var latest = (await preview.LoadAsync(mailboxAddress, lifetime.Token)).Messages.Last(m => !m.IsOutgoing);
            await HandleNotificationActionAsync("read", new(mailboxAddress, latest.PeerKey, latest.ThreadId, latest.MessageId), null);
            var afterRead = await preview.LoadAsync(mailboxAddress, lifetime.Token);
            Check(afterRead.Messages.Where(m => !m.IsOutgoing && m.PeerKey == "/preview/design").All(m => m.Read == true), "Mark as read acknowledges the whole conversation");
            Check((await AppNotificationManager.Default.GetAllAsync()).All(a => a.Group != designGroup), "Mark as read removes that conversation's toasts");
            Check(!AppWindow.IsVisible, "Mark as read leaves the app in the tray");
            preview.ReceiveForUiTest("design-release");
            await ViewModel.RefreshAsync();
            latest = (await preview.LoadAsync(mailboxAddress, lifetime.Token)).Messages.Last(m => !m.IsOutgoing);
            await HandleNotificationActionAsync("reply", new(mailboxAddress, latest.PeerKey, latest.ThreadId, latest.MessageId), "Replying from the toast");
            var afterReply = await preview.LoadAsync(mailboxAddress, lifetime.Token);
            var sent = afterReply.Messages.Last(m => m.IsOutgoing);
            Check(RequestEnvelope.DisplayText(sent.Body) == "Replying from the toast" && sent.ThreadId == "design-release", "Inline reply is sent into the toast's subject");
            Check(!AppWindow.IsVisible, "Replying leaves the app in the tray");

            unreadState.Clear(); notifications!.Update(0);
            await DesktopNotifications.ClearAlertsAsync();
            Check(unreadState.Count == 0 && (await AppNotificationManager.Default.GetAllAsync()).Count == 0, "Sign-out cleanup clears the badge and native alerts");
            File.WriteAllText(resultPath, System.Text.Json.JsonSerializer.Serialize(new { passed = true, checks, toast = toastPayload, identity, shell = $"0x{shell:X8}", setting = setting.ToString(), taskbarWarning = notifications.TaskbarWarning }));
        }
        catch (Exception ex)
        {
            DesktopLog.Error("Notification integration failed", ex);
            File.WriteAllText(resultPath, System.Text.Json.JsonSerializer.Serialize(new { passed = false, checks, identity = DesktopLog.PackageIdentity(), error = ex.ToString() }));
        }
        finally { Quit(); }
    }

    private static void CaptureScreen(string path)
    {
        try
        {
            var width = GetSystemMetrics(0); var height = GetSystemMetrics(1);
            using var bitmap = new System.Drawing.Bitmap(width, height);
            using (var graphics = System.Drawing.Graphics.FromImage(bitmap)) graphics.CopyFromScreen(0, 0, 0, 0, bitmap.Size);
            bitmap.Save(path, System.Drawing.Imaging.ImageFormat.Png);
        }
        catch (Exception ex) { DesktopLog.Error("Capturing the screen failed", ex); }
    }

    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern int GetSystemMetrics(int index);
}

#endif
