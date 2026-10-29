using Microsoft.Windows.AppLifecycle;
using Microsoft.Windows.AppNotifications;
using Microsoft.Windows.PushNotifications;
using Pigeonpost.Core;

namespace Pigeonpost.Desktop;

//  Windows App SDK push (Microsoft.Windows.PushNotifications) for incoming calls while the app is
//  closed (docs/call-alerting.md). The postbox authenticates as the Entra app WnsChannel.AzureAppId
//  and sends WNS toasts (wns/toast) to the channel created here; Windows shows them and activates
//  the app through AppNotificationManager, exactly like the app's own call toasts.
//
//  The end of a ring is a raw push, {"type":"call_cancel","room":…} (WNS cannot remove a toast on
//  a desktop channel from the server). While the app runs it arrives on PushReceived; while it is
//  closed Windows starts the app in the background for it (App.OnLaunched), which removes the
//  ring's toast by tag and group and exits without a window.
//
//  The older PushNotificationChannelManager channel is tied to the Store package SID, whose
//  login.live.com credentials Microsoft no longer issues for new apps.
//
//  A failed channel request is logged and calls then ring only while the app runs.
internal static class WindowsPush
{
    private static bool Registered { get; set; }
    private static Task<PushNotificationChannel?>? pending;

    // A ring ended by push (the app's ring state follows; the toast is already removed here).
    public static event Action<string>? RingCancelled;
    private static DateTimeOffset requestedAt;
    private static readonly TimeSpan ReuseFor = TimeSpan.FromHours(1);

    // Packaged builds only, once, before AppInstance.GetActivatedEventArgs (a Windows App SDK rule).
    public static void Register()
    {
#if !UI_TESTS
        try
        {
            if (!PushNotificationManager.IsSupported()) { DesktopLog.Write("Windows push unsupported on this PC"); return; }
            // Handlers must be attached before Register.
            PushNotificationManager.Default.PushReceived += (sender, e) => _ = HandleRawAsync(e.Payload, "push");
            PushNotificationManager.Default.Register();
            Registered = true;
            DesktopLog.Write("Windows push registered");
        }
        catch (Exception ex) { DesktopLog.Error("Windows push registration failed", ex); }
#endif
    }

    // Windows started the app for a raw push while it was closed.
    public static async Task HandleBackgroundAsync(AppActivationArguments activation)
    {
        if (activation.Data is not PushNotificationReceivedEventArgs push) return;
        var deferral = push.GetDeferral();
        try { await HandleRawAsync(push.Payload, "background push"); }
        catch (Exception ex) { DesktopLog.Error("Handling a background push failed", ex); }
        finally { deferral.Complete(); }
    }

    private static async Task HandleRawAsync(byte[] payload, string how)
    {
        if (CallPush.CancelledRoom(payload) is not { } room) { DesktopLog.Write($"Raw push ignored ({payload.Length} bytes)"); return; }
        DesktopLog.Write($"Ring ended ({how})");
        try { await AppNotificationManager.Default.RemoveByTagAndGroupAsync(CallToast.Tag(room), CallToast.Group); }
        catch (Exception ex) { DesktopLog.Error("Removing the call alert failed", ex); }
        RingCancelled?.Invoke(room);
    }

    // One request at a time. Windows App SDK retries retryable failures by itself for up to 15
    // minutes, so a later caller joins the request in flight rather than starting another. A
    // channel obtained in the last hour (the one requested at launch, used at sign-in) is reused;
    // the daily refresh asks again.
    public static Task<PushNotificationChannel?> CreateChannelAsync()
    {
        if (pending is { IsCompleted: false }) return pending;
        if (pending is { IsCompletedSuccessfully: true, Result: not null } && DateTimeOffset.UtcNow - requestedAt < ReuseFor) return pending;
        requestedAt = DateTimeOffset.UtcNow;
        return pending = CreateAsync();
    }

    // Sign-out closes the channel; the next sign-in asks for a new one.
    public static void Forget() => pending = null;

    private static async Task<PushNotificationChannel?> CreateAsync()
    {
        if (!Registered) return null;
        try
        {
            var operation = PushNotificationManager.Default.CreateChannelAsync(WnsChannel.AzureObjectId);
            operation.Progress = (_, progress) =>
            {
                if (progress.status == PushNotificationChannelStatus.InProgressRetry)
                    DesktopLog.Write($"Push channel request retrying (attempt {progress.retryCount}, {Code(progress.extendedError)})");
            };
            var result = await operation;
            if (result.Status == PushNotificationChannelStatus.CompletedSuccess && result.Channel is { } channel)
            {
                // The host only: the channel's token addresses this PC and is kept out of the log.
                DesktopLog.Write($"Push channel ready ({channel.Uri.Host}); expires {channel.ExpirationTime:yyyy-MM-dd}");
                return channel;
            }
            DesktopLog.Write($"Push channel unavailable: {result.Status} {Code(result.ExtendedError)}");
        }
        catch (Exception ex) { DesktopLog.Error("Push channel request failed", ex); }
        return null;
    }

    // HRESULTs project to Exception in C#/WinRT; keep only the code.
    private static string Code(object? error) => error switch
    {
        Exception ex => $"0x{ex.HResult:X8}",
        int hr => $"0x{hr:X8}",
        _ => "no error code",
    };
}
