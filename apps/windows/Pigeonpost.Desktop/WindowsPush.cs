using Microsoft.Windows.PushNotifications;
using Pigeonpost.Core;

namespace Pigeonpost.Desktop;

//  Windows App SDK push (Microsoft.Windows.PushNotifications) for incoming calls while the app is
//  closed (docs/call-alerting.md). The postbox authenticates as the Entra app WnsChannel.AzureAppId
//  and sends WNS toasts (wns/toast) to the channel created here; Windows shows them and activates
//  the app through AppNotificationManager, exactly like the app's own call toasts. The postbox
//  sends no raw pushes: one would only be logged, and a push activation starts the app in the
//  notification area.
//
//  The older PushNotificationChannelManager channel is tied to the Store package SID, whose
//  login.live.com credentials Microsoft no longer issues for new apps.
//
//  Channels need Microsoft's mapping of this package family name to the Entra app. Until it exists
//  the request fails; that is logged and calls ring only while the app runs.
internal static class WindowsPush
{
    private static bool Registered { get; set; }
    private static Task<PushNotificationChannel?>? pending;
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
            PushNotificationManager.Default.PushReceived += (_, e) => DesktopLog.Write($"Raw push ignored ({e.Payload.Length} bytes)");
            PushNotificationManager.Default.Register();
            Registered = true;
            DesktopLog.Write("Windows push registered");
        }
        catch (Exception ex) { DesktopLog.Error("Windows push registration failed", ex); }
#endif
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
