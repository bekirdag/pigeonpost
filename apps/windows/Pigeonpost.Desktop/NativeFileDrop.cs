using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;
using System.Text;

namespace Pigeonpost.Desktop;

// WinUI's external drag pipeline can reject OLE files before raising XAML events.
// Own the native file target on our content windows and pass copied paths to the
// same bounded, conversation-scoped staging path used by the picker and clipboard.
[ComVisible(true), ClassInterface(ClassInterfaceType.None)]
public sealed class NativeFileDrop : INativeFileDropTarget, IDisposable
{
    private readonly List<IntPtr> windows = [];
    private readonly Func<int, int, bool> accepts;
    private readonly Action<string[]> receive;
    private bool files;
    private bool disposed;

    public NativeFileDrop(IntPtr window, Func<int, int, bool> accepts, Action<string[]> receive)
    {
        this.accepts = accepts;
        this.receive = receive;
        Marshal.ThrowExceptionForHR(OleInitialize(IntPtr.Zero));
        try
        {
            Register(window);
            EnumWindowsCallback callback = (child, _) => { Register(child); return true; };
            EnumChildWindows(window, callback, IntPtr.Zero);
            GC.KeepAlive(callback);
        }
        catch { Dispose(); throw; }
    }

    private void Register(IntPtr window)
    {
        var result = RegisterDragDrop(window, this);
        if (result == unchecked((int)0x80040101)) // Our WinUI content window already registered itself.
        {
            Marshal.ThrowExceptionForHR(RevokeDragDrop(window));
            result = RegisterDragDrop(window, this);
        }
        Marshal.ThrowExceptionForHR(result);
        windows.Add(window);
    }

    private static FORMATETC FileFormat() => new()
    { cfFormat = 15, dwAspect = DVASPECT.DVASPECT_CONTENT, lindex = -1, tymed = TYMED.TYMED_HGLOBAL };

    public int DragEnter(IDataObject data, uint keys, DropPoint point, ref uint effect)
    {
        try { var format = FileFormat(); files = data.QueryGetData(ref format) == 0; }
        catch { files = false; }
        return DragOver(keys, point, ref effect);
    }

    public int DragOver(uint keys, DropPoint point, ref uint effect)
    {
        try { effect = files && !disposed && accepts(point.X, point.Y) ? effect & 1u : 0; }
        catch { effect = 0; }
        return 0;
    }

    public int DragLeave() { files = false; return 0; }

    public int Drop(IDataObject data, uint keys, DropPoint point, ref uint effect)
    {
        DragOver(keys, point, ref effect);
        if (effect == 0) return 0;
        try
        {
            var format = FileFormat();
            data.GetData(ref format, out var medium);
            try
            {
                var count = DragQueryFile(medium.unionmember, uint.MaxValue, null, 0);
                if (count == 0 || count > 1000) { effect = 0; return 0; }
                var paths = new List<string>();
                for (uint i = 0; i < count; i++)
                {
                    var length = DragQueryFile(medium.unionmember, i, null, 0);
                    if (length == 0 || length > 32767) { effect = 0; return 0; }
                    var path = new StringBuilder((int)length + 1);
                    DragQueryFile(medium.unionmember, i, path, (uint)path.Capacity);
                    paths.Add(path.ToString());
                }
                receive(paths.ToArray());
            }
            finally { ReleaseStgMedium(ref medium); }
        }
        catch { effect = 0; }
        finally { files = false; }
        return 0;
    }

    public void Dispose()
    {
        if (disposed) return;
        disposed = true;
        foreach (var window in windows) RevokeDragDrop(window);
        windows.Clear();
        OleUninitialize();
    }

    private delegate bool EnumWindowsCallback(IntPtr window, IntPtr state);
    [DllImport("ole32.dll")] private static extern int OleInitialize(IntPtr reserved);
    [DllImport("ole32.dll")] private static extern void OleUninitialize();
    [DllImport("ole32.dll")] private static extern int RegisterDragDrop(IntPtr window, [MarshalAs(UnmanagedType.Interface)] INativeFileDropTarget target);
    [DllImport("ole32.dll")] private static extern int RevokeDragDrop(IntPtr window);
    [DllImport("ole32.dll")] private static extern void ReleaseStgMedium(ref STGMEDIUM medium);
    [DllImport("user32.dll")] private static extern bool EnumChildWindows(IntPtr window, EnumWindowsCallback callback, IntPtr state);
    [DllImport("shell32.dll", CharSet = CharSet.Unicode, EntryPoint = "DragQueryFileW")]
    private static extern uint DragQueryFile(IntPtr drop, uint index, StringBuilder? path, uint length);
    [DllImport("user32.dll")] internal static extern bool ClientToScreen(IntPtr window, ref DropPoint point);
}

[StructLayout(LayoutKind.Sequential)]
public struct DropPoint { public int X; public int Y; }

[ComVisible(true), Guid("00000122-0000-0000-C000-000000000046"), InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
public interface INativeFileDropTarget
{
    [PreserveSig] int DragEnter([MarshalAs(UnmanagedType.Interface)] IDataObject data, uint keys, DropPoint point, ref uint effect);
    [PreserveSig] int DragOver(uint keys, DropPoint point, ref uint effect);
    [PreserveSig] int DragLeave();
    [PreserveSig] int Drop([MarshalAs(UnmanagedType.Interface)] IDataObject data, uint keys, DropPoint point, ref uint effect);
}
