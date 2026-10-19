using System.ComponentModel;
using System.Runtime.InteropServices;

namespace Pigeonpost.Desktop;

// The notification-area icon, owned directly through Shell_NotifyIcon so every shell result is
// checked and logged. A stable GUID lets Windows remember "show this icon" across launches; if
// Windows refuses it (for example after the package moved to a new versioned folder), the icon
// is added by window and id instead so it is never missing. Explorer restarts re-add it.
internal sealed class TrayIcon : IDisposable
{
    private static readonly Guid IconGuid = new("6F3B7C2E-4D1A-4E8B-9C35-2A7D5E1F0B64");
    private const uint CallbackMessage = 0x8001; // WM_APP + 1
    private const uint IconId = 1;
    private readonly WindowProcedure procedure;
    private readonly IntPtr window;
    private readonly uint taskbarCreated;
    private readonly Action open;
    private readonly Action quit;
    private IntPtr icon;
    private string tip = "Pigeonpost";
    private bool useGuid = true;
    private bool disposed;

    public bool IsAdded { get; private set; }

    public TrayIcon(IntPtr icon, Action open, Action quit)
    {
        this.icon = icon;
        this.open = open;
        this.quit = quit;
        procedure = WndProc;
        var className = "PigeonpostTray" + Environment.ProcessId;
        var windowClass = new WindowClass
        {
            Size = (uint)Marshal.SizeOf<WindowClass>(), Procedure = Marshal.GetFunctionPointerForDelegate(procedure),
            Instance = GetModuleHandle(null), ClassName = className
        };
        if (RegisterClassEx(ref windowClass) == 0) throw new Win32Exception(Marshal.GetLastWin32Error(), "Tray window class");
        // A hidden top-level window (not message-only) so the TaskbarCreated broadcast reaches it.
        window = CreateWindowEx(0, className, "Pigeonpost notification area", 0, 0, 0, 0, 0, IntPtr.Zero, IntPtr.Zero, windowClass.Instance, IntPtr.Zero);
        if (window == IntPtr.Zero) throw new Win32Exception(Marshal.GetLastWin32Error(), "Tray window");
        taskbarCreated = RegisterWindowMessage("TaskbarCreated");
        ChangeWindowMessageFilterEx(window, taskbarCreated, 1, IntPtr.Zero); // Allow the broadcast into elevated processes.
        Add();
    }

    public void Update(IntPtr newIcon, string newTip)
    {
        icon = newIcon;
        tip = newTip.Length > 127 ? newTip[..127] : newTip;
        if (!IsAdded) { Add(); return; }
        var data = Data(NotifyFlags.Icon | NotifyFlags.Tip | NotifyFlags.ShowTip);
        if (!Shell_NotifyIcon(NotifyMessage.Modify, ref data))
        {
            DesktopLog.Write($"Notification-area icon update refused (error {Marshal.GetLastWin32Error()}); adding it again");
            IsAdded = false;
            Add();
        }
    }

    // S_OK when the shell holds the icon; logged and asserted by the packaged CI check.
    public int ShellState()
    {
        var identifier = new NotifyIconIdentifier { Size = (uint)Marshal.SizeOf<NotifyIconIdentifier>() };
        if (useGuid) identifier.Item = IconGuid;
        else { identifier.Window = window; identifier.Id = IconId; }
        return Shell_NotifyIconGetRect(ref identifier, out _);
    }

    private void Add()
    {
        if (disposed) return;
        var data = Data(NotifyFlags.Message | NotifyFlags.Icon | NotifyFlags.Tip | NotifyFlags.ShowTip);
        var added = Shell_NotifyIcon(NotifyMessage.Add, ref data);
        if (!added && useGuid)
        {
            // A previous process may have exited without removing the icon: clear it and retry.
            var stale = Data(0);
            Shell_NotifyIcon(NotifyMessage.Delete, ref stale);
            added = Shell_NotifyIcon(NotifyMessage.Add, ref data);
            if (!added)
            {
                DesktopLog.Write($"Notification-area icon with a stable identity refused (error {Marshal.GetLastWin32Error()}); using a per-window icon");
                useGuid = false;
                data = Data(NotifyFlags.Message | NotifyFlags.Icon | NotifyFlags.Tip | NotifyFlags.ShowTip);
                added = Shell_NotifyIcon(NotifyMessage.Add, ref data);
            }
        }
        IsAdded = added;
        if (!added) { DesktopLog.Write($"Notification-area icon could not be added (error {Marshal.GetLastWin32Error()})"); return; }
        data.TimeoutOrVersion = 4; // NOTIFYICON_VERSION_4
        Shell_NotifyIcon(NotifyMessage.SetVersion, ref data);
        DesktopLog.Write($"Notification-area icon added ({(useGuid ? "stable identity" : "per-window")}); shell reports 0x{ShellState():X8}");
    }

    private NotifyIconData Data(NotifyFlags flags)
    {
        var data = new NotifyIconData
        {
            Size = (uint)Marshal.SizeOf<NotifyIconData>(), Window = window, Id = IconId, Flags = flags,
            CallbackMessage = CallbackMessage, Icon = icon, Tip = tip
        };
        if (useGuid) { data.Flags |= NotifyFlags.Guid; data.Item = IconGuid; }
        return data;
    }

    private IntPtr WndProc(IntPtr hwnd, uint message, IntPtr wParam, IntPtr lParam)
    {
        try
        {
            if (message == taskbarCreated && taskbarCreated != 0)
            {
                DesktopLog.Write("Explorer restarted; adding the notification-area icon again");
                IsAdded = false;
                Add();
                return IntPtr.Zero;
            }
            if (message == CallbackMessage)
            {
                switch ((uint)(lParam.ToInt64() & 0xFFFF))
                {
                    case 0x0400: case 0x0401: case 0x0202: open(); break; // NIN_SELECT, NIN_KEYSELECT, WM_LBUTTONUP
                    case 0x007B: case 0x0205: ShowMenu(); break;          // WM_CONTEXTMENU, WM_RBUTTONUP
                }
                return IntPtr.Zero;
            }
        }
        catch (Exception ex) { DesktopLog.Error("Notification-area icon input failed", ex); }
        return DefWindowProc(hwnd, message, wParam, lParam);
    }

    private void ShowMenu()
    {
        var menu = CreatePopupMenu();
        try
        {
            AppendMenu(menu, 0, (UIntPtr)1, "Open Pigeonpost");
            AppendMenu(menu, 0, (UIntPtr)2, "Quit Pigeonpost");
            GetCursorPos(out var point);
            SetForegroundWindow(window); // Required so the menu closes when the user clicks elsewhere.
            var command = TrackPopupMenuEx(menu, 0x0100 | 0x0002 | 0x0080, point.X, point.Y, window, IntPtr.Zero); // RETURNCMD | RIGHTBUTTON | NONOTIFY
            PostMessage(window, 0, IntPtr.Zero, IntPtr.Zero);
            if (command == 1) open();
            else if (command == 2) quit();
        }
        finally { DestroyMenu(menu); }
    }

    public void Dispose()
    {
        if (disposed) return;
        disposed = true;
        var data = Data(0);
        if (IsAdded) Shell_NotifyIcon(NotifyMessage.Delete, ref data);
        IsAdded = false;
        DestroyWindow(window);
    }

    private delegate IntPtr WindowProcedure(IntPtr hwnd, uint message, IntPtr wParam, IntPtr lParam);
    private enum NotifyMessage : uint { Add = 0, Modify = 1, Delete = 2, SetVersion = 4 }
    [Flags] private enum NotifyFlags : uint { Message = 1, Icon = 2, Tip = 4, Guid = 0x20, ShowTip = 0x80 }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct NotifyIconData
    {
        public uint Size; public IntPtr Window; public uint Id; public NotifyFlags Flags; public uint CallbackMessage; public IntPtr Icon;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 128)] public string Tip;
        public uint State; public uint StateMask;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 256)] public string Info;
        public uint TimeoutOrVersion;
        [MarshalAs(UnmanagedType.ByValTStr, SizeConst = 64)] public string InfoTitle;
        public uint InfoFlags; public Guid Item; public IntPtr BalloonIcon;
    }

    [StructLayout(LayoutKind.Sequential)]
    private struct NotifyIconIdentifier { public uint Size; public IntPtr Window; public uint Id; public Guid Item; }
    [StructLayout(LayoutKind.Sequential)] private struct NativeRect { public int Left, Top, Right, Bottom; }
    [StructLayout(LayoutKind.Sequential)] private struct NativePoint { public int X, Y; }

    [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
    private struct WindowClass
    {
        public uint Size; public uint Style; public IntPtr Procedure; public int ClassExtra; public int WindowExtra;
        public IntPtr Instance; public IntPtr Icon; public IntPtr Cursor; public IntPtr Background;
        public string? MenuName; public string ClassName; public IntPtr SmallIcon;
    }

    [DllImport("shell32.dll", CharSet = CharSet.Unicode, SetLastError = true)] private static extern bool Shell_NotifyIcon(NotifyMessage message, ref NotifyIconData data);
    [DllImport("shell32.dll")] private static extern int Shell_NotifyIconGetRect(ref NotifyIconIdentifier identifier, out NativeRect location);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)] private static extern ushort RegisterClassEx(ref WindowClass windowClass);
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr CreateWindowEx(uint exStyle, string className, string title, uint style, int x, int y, int width, int height, IntPtr parent, IntPtr menu, IntPtr instance, IntPtr parameter);
    [DllImport("user32.dll")] private static extern bool DestroyWindow(IntPtr window);
    [DllImport("user32.dll")] private static extern IntPtr DefWindowProc(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern uint RegisterWindowMessage(string name);
    [DllImport("user32.dll")] private static extern bool ChangeWindowMessageFilterEx(IntPtr window, uint message, uint action, IntPtr status);
    [DllImport("user32.dll")] private static extern IntPtr CreatePopupMenu();
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern bool AppendMenu(IntPtr menu, uint flags, UIntPtr id, string text);
    [DllImport("user32.dll")] private static extern bool DestroyMenu(IntPtr menu);
    [DllImport("user32.dll")] private static extern uint TrackPopupMenuEx(IntPtr menu, uint flags, int x, int y, IntPtr window, IntPtr parameters);
    [DllImport("user32.dll")] private static extern bool SetForegroundWindow(IntPtr window);
    [DllImport("user32.dll")] private static extern bool PostMessage(IntPtr window, uint message, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll")] private static extern bool GetCursorPos(out NativePoint point);
    [DllImport("kernel32.dll", CharSet = CharSet.Unicode)] private static extern IntPtr GetModuleHandle(string? name);
}
