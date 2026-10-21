using System.Drawing;
using System.Drawing.Drawing2D;
using System.Runtime.InteropServices;
using Microsoft.Windows.AppNotifications;
using Microsoft.Windows.AppNotifications.Builder;
using Pigeonpost.Core;

namespace Pigeonpost.Desktop;

// Windows owns alert permissions; unread indicators continue to work if alerts are disabled.
internal sealed class DesktopNotifications : IDisposable
{
    private readonly IntPtr window;
    private readonly TrayIcon tray;
    private readonly Icon normal;
    private Icon? badge;
    private ITaskbarList3? taskbar;
    private int count = -1;
    // Hide to the tray only when the shell really shows our icon; otherwise closing would
    // leave a running app with no window and no way back.
    public bool CanHide => tray.IsAdded;
    internal int UnreadCount => count;
    internal string? TaskbarWarning { get; private set; }

    public DesktopNotifications(IntPtr window, Action show, Action quit)
    {
        this.window = window;
        normal = new Icon(Path.Combine(AppContext.BaseDirectory, "Assets", "Pigeonpost.ico"));
        tray = new TrayIcon(normal.Handle, show, quit);
        try { taskbar = (ITaskbarList3)new TaskbarList(); taskbar.HrInit(); }
        catch (Exception ex) when (ex is COMException or UnauthorizedAccessException)
        { taskbar = null; TaskbarWarning = ex.Message; }
        Update(0);
    }

    public void Update(int unread, bool force = false)
    {
        unread = Math.Max(0, unread);
        if (count == unread && !force) return;
        count = unread;
        var previous = badge;
        badge = unread == 0 ? null : CreateBadge(unread);
        var text = unread == 0 ? "Pigeonpost — no unread messages" : $"Pigeonpost — {unread} unread messages";
        tray.Update((badge ?? normal).Handle, text);
        try
        {
            taskbar?.SetOverlayIcon(window, badge?.Handle ?? IntPtr.Zero, text);
            if (taskbar is not null) TaskbarWarning = null;
        }
        catch (Exception ex) when (ex is COMException or UnauthorizedAccessException)
        {
            // Explorer can be restarting or owned by another user (CI). Keep the
            // tray and alerts alive; the next activation retries the shell overlay.
            TaskbarWarning = ex.Message;
        }
        previous?.Dispose();
    }

    // A real Windows toast, which Windows places bottom-right and files in the notification centre:
    // the sender's name and face, a preview the user's setting allows, an inline Reply box and
    // Mark as read. Clicking the body opens the conversation. Every action carries the mailbox, peer
    // and message, and App re-checks them against the signed-in account before doing anything.
    public static bool Announce(MessageArrival arrival, MessageAlert alert, string? mailboxName = null)
    {
        try
        {
            AppNotificationButton Action(string label, string action) => new AppNotificationButton(label)
                .AddArgument("action", action).AddArgument("mailbox", arrival.Mailbox).AddArgument("peer", arrival.Peer)
                .AddArgument("thread", arrival.ThreadId ?? "").AddArgument("message", arrival.MessageId);
            var builder = new AppNotificationBuilder()
                .AddArgument("action", "open")
                .AddArgument("mailbox", arrival.Mailbox).AddArgument("peer", arrival.Peer)
                .AddArgument("thread", arrival.ThreadId ?? "").AddArgument("message", arrival.MessageId)
                .AddText(alert.Title)
                .AddText(alert.Text)
                .SetTimeStamp(DateTime.Now)
                .AddTextBox(ReplyInput, "Reply", "")
                .AddButton(Action("Reply", "reply").SetInputId(ReplyInput))
                .AddButton(Action("Mark as read", "read"));
            if (!string.IsNullOrEmpty(mailboxName)) builder.SetAttributionText("To " + mailboxName);
            // The sender's profile picture when the app has shown it (Phase 1b, SDS §16), else initials.
            if (alert.Title != MessageAlerts.GenericTitle && (AvatarFiles.For(arrival.Peer) ?? AvatarUri(alert)) is { } avatar)
                builder.SetAppLogoOverride(avatar, AppNotificationImageCrop.Circle);
            var notification = builder.BuildNotification();
            notification.Tag = alert.Tag;
            notification.Group = alert.Group;
            AppNotificationManager.Default.Show(notification);
            if (notification.Id == 0) DesktopLog.Write($"Windows did not accept an alert; setting {AppNotificationManager.Default.Setting}");
            return notification.Id != 0;
        }
        catch (Exception ex) { DesktopLog.Error("Showing an alert failed", ex); return false; }
    }

    internal const string ReplyInput = "reply";

    // One conversation's toasts leave the notification centre together once it has been read.
    public static async Task RemoveConversationAsync(string mailbox, string peer)
    {
        try { await AppNotificationManager.Default.RemoveByGroupAsync(MessageAlerts.GroupKey(mailbox, peer)); }
        catch (Exception ex) { DesktopLog.Error("Removing a conversation's alerts failed", ex); }
    }

    // The sender's initials on a disc, written where Windows can read it: the package's temporary
    // folder when installed, the user's temp folder in an unpackaged run.
    private static Uri? AvatarUri(MessageAlert alert)
    {
        try
        {
            string folder; Func<string, Uri> uri;
            try
            {
                folder = Path.Combine(Windows.Storage.ApplicationData.Current.TemporaryFolder.Path, "avatars");
                uri = name => new Uri("ms-appdata:///temp/avatars/" + name);
            }
            catch (InvalidOperationException)
            {
                folder = Path.Combine(Path.GetTempPath(), "Pigeonpost", "avatars");
                uri = name => new Uri(Path.Combine(folder, name));
            }
            Directory.CreateDirectory(folder);
            var file = MessageAlerts.Key(alert.Initials + "\n" + alert.Group) + ".png";
            var path = Path.Combine(folder, file);
            if (!File.Exists(path))
            {
                using var bitmap = new Bitmap(96, 96);
                using (var graphics = Graphics.FromImage(bitmap))
                {
                    graphics.SmoothingMode = SmoothingMode.AntiAlias;
                    graphics.TextRenderingHint = System.Drawing.Text.TextRenderingHint.AntiAliasGridFit;
                    graphics.Clear(Color.Transparent);
                    var tones = new[] { Color.FromArgb(22, 50, 107), Color.FromArgb(37, 99, 235), Color.FromArgb(15, 118, 110),
                        Color.FromArgb(124, 58, 237), Color.FromArgb(180, 83, 9), Color.FromArgb(190, 18, 60) };
                    using var background = new SolidBrush(tones[Convert.ToInt32(alert.Group[..2], 16) % tones.Length]);
                    graphics.FillEllipse(background, 0, 0, 96, 96);
                    using var font = new Font("Segoe UI", alert.Initials.Length > 2 ? 30 : 38, FontStyle.Bold, GraphicsUnit.Pixel);
                    using var format = new StringFormat { Alignment = StringAlignment.Center, LineAlignment = StringAlignment.Center };
                    graphics.DrawString(alert.Initials, font, Brushes.White, new RectangleF(0, 2, 96, 96), format);
                }
                var temporary = path + ".tmp";
                bitmap.Save(temporary, System.Drawing.Imaging.ImageFormat.Png);
                File.Move(temporary, path, overwrite: true);
            }
            return uri(file);
        }
        catch (Exception ex) { DesktopLog.Error("Drawing a toast avatar failed", ex); return null; }
    }

    public static bool ShowTrayHint()
    {
        try
        {
            var notification = new AppNotificationBuilder().AddText("Pigeonpost is still running in the notification area").BuildNotification();
            notification.Tag = "tray-hint";
            AppNotificationManager.Default.Show(notification);
            return notification.Id != 0;
        }
        catch (Exception ex) { DesktopLog.Error("Showing the notification-area hint failed", ex); return false; }
    }

    public static async Task ClearAlertsAsync()
    {
        try { await AppNotificationManager.Default.RemoveAllAsync(); }
        catch (Exception) { /* Sign-out still clears local state when notifications are unavailable. */ }
    }

    private static Icon CreateBadge(int unread)
    {
        using var bitmap = new Bitmap(32, 32);
        using (var graphics = Graphics.FromImage(bitmap))
        {
            graphics.SmoothingMode = SmoothingMode.AntiAlias;
            graphics.Clear(Color.Transparent);
            using var background = new SolidBrush(Color.FromArgb(196, 36, 53));
            graphics.FillEllipse(background, 0, 0, 32, 32);
            var text = unread > 99 ? "99+" : unread.ToString();
            using var font = new Font("Segoe UI", text.Length > 2 ? 12 : 16, FontStyle.Bold, GraphicsUnit.Pixel);
            using var format = new StringFormat { Alignment = StringAlignment.Center, LineAlignment = StringAlignment.Center };
            graphics.DrawString(text, font, Brushes.White, new RectangleF(0, 0, 32, 32), format);
        }
        var handle = bitmap.GetHicon();
        try { return (Icon)Icon.FromHandle(handle).Clone(); }
        finally { DestroyIcon(handle); }
    }

    public void Dispose()
    {
        try { taskbar?.SetOverlayIcon(window, IntPtr.Zero, ""); }
        catch (Exception ex) when (ex is COMException or UnauthorizedAccessException) { }
        tray.Dispose(); badge?.Dispose(); normal.Dispose();
        if (taskbar is not null) Marshal.FinalReleaseComObject(taskbar);
    }

    internal int ShellIconState() => tray.ShellState();

    [DllImport("user32.dll")] private static extern bool DestroyIcon(IntPtr icon);
}

[ComImport, Guid("56FDF344-FD6D-11D0-958A-006097C9A090")]
internal class TaskbarList { }

// Preserve the native vtable order through SetOverlayIcon (ITaskbarList + 2 + 3).
[ComImport, Guid("EA1AFB91-9E28-4B86-90E9-9E9F8A5EEFAF"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
internal interface ITaskbarList3
{
    void HrInit(); void AddTab(IntPtr hwnd); void DeleteTab(IntPtr hwnd); void ActivateTab(IntPtr hwnd); void SetActiveAlt(IntPtr hwnd);
    void MarkFullscreenWindow(IntPtr hwnd, [MarshalAs(UnmanagedType.Bool)] bool fullscreen);
    void SetProgressValue(IntPtr hwnd, ulong completed, ulong total); void SetProgressState(IntPtr hwnd, uint flags);
    void RegisterTab(IntPtr tab, IntPtr mdi); void UnregisterTab(IntPtr tab); void SetTabOrder(IntPtr tab, IntPtr before);
    void SetTabActive(IntPtr tab, IntPtr mdi, uint flags); void ThumbBarAddButtons(IntPtr hwnd, uint count, IntPtr buttons);
    void ThumbBarUpdateButtons(IntPtr hwnd, uint count, IntPtr buttons); void ThumbBarSetImageList(IntPtr hwnd, IntPtr images);
    void SetOverlayIcon(IntPtr hwnd, IntPtr icon, [MarshalAs(UnmanagedType.LPWStr)] string description);
}
