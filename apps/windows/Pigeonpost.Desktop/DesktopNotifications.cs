using System.Drawing;
using System.Drawing.Drawing2D;
using System.Runtime.InteropServices;
using Microsoft.Windows.AppNotifications;
using Microsoft.Windows.AppNotifications.Builder;
using Pigeonpost.Core;
using Forms = System.Windows.Forms;

namespace Pigeonpost.Desktop;

// Windows owns alert permissions; unread indicators continue to work if alerts are disabled.
internal sealed class DesktopNotifications : IDisposable
{
    private readonly IntPtr window;
    private readonly Forms.NotifyIcon tray;
    private readonly Icon normal;
    private Icon? badge;
    private ITaskbarList3? taskbar;
    private int count = -1;
    public bool CanHide => tray.Visible;
    internal int UnreadCount => count;
    internal string? TaskbarWarning { get; private set; }

    public DesktopNotifications(IntPtr window, Action show, Action quit)
    {
        this.window = window;
        normal = new Icon(Path.Combine(AppContext.BaseDirectory, "Assets", "Pigeonpost.ico"));
        var menu = new Forms.ContextMenuStrip();
        menu.Items.Add("Open Pigeonpost", null, (_, _) => show());
        menu.Items.Add("Quit Pigeonpost", null, (_, _) => quit());
        tray = new Forms.NotifyIcon { Icon = normal, Text = "Pigeonpost", ContextMenuStrip = menu, Visible = true };
        tray.MouseClick += (_, e) => { if (e.Button == Forms.MouseButtons.Left) show(); };
        DesktopLog.Write($"Notification-area icon requested; shell reports 0x{ShellIconState():X8}");
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
        tray.Icon = badge ?? normal;
        tray.Text = unread == 0 ? "Pigeonpost — no unread messages" : $"Pigeonpost — {unread} unread messages";
        try
        {
            taskbar?.SetOverlayIcon(window, badge?.Handle ?? IntPtr.Zero, tray.Text);
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

    public static bool Announce(MessageArrival arrival)
    {
        try
        {
            var notification = new AppNotificationBuilder()
                .AddArgument("mailbox", arrival.Mailbox).AddArgument("peer", arrival.Peer)
                .AddArgument("thread", arrival.ThreadId ?? "").AddArgument("message", arrival.MessageId)
                .AddText("New Pigeonpost message")
                .AddText("Open Pigeonpost to read your message.").BuildNotification();
            notification.Tag = NotificationKey(arrival.MessageId);
            notification.Group = NotificationKey(arrival.Mailbox);
            AppNotificationManager.Default.Show(notification);
            if (notification.Id == 0) DesktopLog.Write($"Windows did not accept an alert; setting {AppNotificationManager.Default.Setting}");
            return notification.Id != 0;
        }
        catch (Exception ex) { DesktopLog.Error("Showing an alert failed", ex); return false; }
    }

    internal static string NotificationKey(string value) => Convert.ToHexString(
        System.Security.Cryptography.SHA256.HashData(System.Text.Encoding.UTF8.GetBytes(value)))[..16];

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
        tray.Visible = false;
        tray.ContextMenuStrip?.Dispose();
        tray.Dispose(); badge?.Dispose(); normal.Dispose();
        if (taskbar is not null) Marshal.FinalReleaseComObject(taskbar);
    }

    // Asks the shell whether it holds our icon (S_OK when registered, visible or in overflow).
    internal int ShellIconState()
    {
        try
        {
            var fields = typeof(Forms.NotifyIcon).GetFields(System.Reflection.BindingFlags.NonPublic | System.Reflection.BindingFlags.Instance);
            var host = fields.Select(f => f.GetValue(tray)).OfType<Forms.NativeWindow>().FirstOrDefault();
            var id = fields.Where(f => f.FieldType == typeof(int) && f.Name.Contains("id", StringComparison.OrdinalIgnoreCase)).Select(f => (int)f.GetValue(tray)!).FirstOrDefault();
            if (host is null || host.Handle == IntPtr.Zero) return unchecked((int)0x80004005);
            var identifier = new NotifyIconIdentifier { Size = (uint)Marshal.SizeOf<NotifyIconIdentifier>(), Window = host.Handle, Id = (uint)id };
            return Shell_NotifyIconGetRect(ref identifier, out _);
        }
        catch (Exception ex) { return ex.HResult; }
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct NotifyIconIdentifier { public uint Size; public IntPtr Window; public uint Id; public Guid Item; }
    [StructLayout(LayoutKind.Sequential)]
    private struct NativeRect { public int Left, Top, Right, Bottom; }
    [DllImport("shell32.dll")] private static extern int Shell_NotifyIconGetRect(ref NotifyIconIdentifier identifier, out NativeRect location);
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
