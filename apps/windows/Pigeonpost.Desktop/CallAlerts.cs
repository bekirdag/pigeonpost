using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.Windows.AppNotifications;
using Pigeonpost.Core;
using Windows.Networking.PushNotifications;

namespace Pigeonpost.Desktop;

//  Incoming-call alerting (docs/call-alerting.md, Phase 1c).
//
//  Where a ring shows:
//  - Window in front (visible, not minimised, active): the call page's own ring card and ringtone,
//    in the window, as before. The page runs with nativeRing: false.
//  - Window in the notification area, minimised or behind other windows: a Windows toast with
//    scenario incomingCall (bottom right, stays until acted on, looping call sound, Answer and
//    Decline). The page runs with nativeRing: true, so it rings silently and shows nothing; the
//    window no longer jumps to the front for a ring. The app re-configures the page whenever the
//    window moves between the two states, and a ring already sounding in the window when it goes
//    to the background moves to a toast.
//  - App not running: the same toast, sent by the postbox through WNS to the channel registered
//    here (when the postbox has WNS credentials). Answer starts the app, signs in from the saved
//    session, opens the call page and answers with the push's ring token; Decline declines with the
//    ring token over HTTP, without the page, and leaves the app in the notification area.
//
//  The local toast and the WNS toast share the room id as tag and "calls" as group, so either
//  replaces the other and the end of the ring removes whichever is showing.
public sealed partial class MainWindow
{
    private readonly CallRingState rings = new();
    private bool callAlertsHooked;
    private bool callWindowActive;
    private bool configuredNativeRing;
    private (string Room, string? RingToken, DateTimeOffset At)? pendingAnswer;
    private DispatcherTimer? ringTimer;
    private PushNotificationChannel? pushChannel;
    private DispatcherTimer? pushTimer;

    private static readonly string PushChannelFile = Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Pigeonpost", "call-push-channel");

    // Once, when calls first start.
    private void HookCallAlerts()
    {
        if (callAlertsHooked) return;
        callAlertsHooked = true;
        callWindowActive = activeWindow; // The window may have been activated before calls started.
        Activated += (sender, e) =>
        {
            callWindowActive = e.WindowActivationState != WindowActivationState.Deactivated;
            _ = UpdateNativeRingAsync();
        };
        AppWindow.Changed += (sender, e) =>
        {
            if (e.DidVisibilityChange || e.DidPresenterChange) _ = UpdateNativeRingAsync();
        };
    }

    private bool CallWindowInFront => callWindowActive && AppWindow.IsVisible
        && AppWindow.Presenter is not OverlappedPresenter { State: OverlappedPresenterState.Minimized };

    // Should the page leave ringing to the app (a toast)? Yes unless the window is in front.
    private bool WantNativeRing
    {
        get
        {
#if DEBUG || UI_TESTS
            // The call harness (test-calls.ps1) checks the in-window ring card on a runner whose
            // window focus is not reliable: it rings in the window unless _NATIVE_RING=1 asks otherwise.
            if (CallsDebug("PAGE") is not null) return CallsDebug("NATIVE_RING") == "1";
#endif
            return !CallWindowInFront;
        }
    }

    private async Task UpdateNativeRingAsync()
    {
        if (!callReady || !callsSignedIn) return;
        var wanted = WantNativeRing;
        if (wanted != configuredNativeRing)
        {
            await ConfigureCallsAsync();
            DesktopLog.Write(wanted ? "Calls ring with Windows alerts" : "Calls ring in the window");
        }
        // A ring sounding in the window when it went to the background now needs a toast.
        // It stays silent: the page already plays its ringtone for a ring it showed itself.
        if (configuredNativeRing)
            foreach (var ring in rings.WithoutToast()) ShowCallToast(ring, silent: true);
    }

    // {type:"ring"} from the page.
    private void CallPage_Ring(CallRing ring)
    {
        if (pendingAnswer is { } answer && answer.Room == ring.Room) return; // Being answered from a toast.
        if (!rings.Begin(ring, DateTimeOffset.UtcNow)) return; // Already ringing (a WNS toast) or already handled here.
        StartRingTimer();
        DesktopLog.Write($"Incoming {ring.Kind} call; {(configuredNativeRing ? "Windows alert" : "ringing in the window")}");
        if (configuredNativeRing) ShowCallToast(ring);
    }

    // {type:"ring_ended"} from the page: answered here or elsewhere, declined, timed out, cancelled.
    private void CallPage_RingEnded(string room)
    {
        rings.End(room);
        _ = RemoveCallToastAsync(room);
    }

    // {type:"call"} from the page: once a call for the room connects, nothing should ring for it.
    private void CallPage_Call(string? state, string? room)
    {
        if (room is null || state is not ("connecting" or "active")) return;
        rings.Handle(room, DateTimeOffset.UtcNow);
        _ = RemoveCallToastAsync(room);
    }

    private string? CallerName(CallRing ring) =>
        ViewModel.AllConversations.FirstOrDefault(c => c.Peer == ring.FromHandle || c.Peer == ring.FromAddress)?.Name;

    private void ShowCallToast(CallRing ring, string? payload = null, bool silent = false)
    {
        try
        {
            var notification = new AppNotification(payload ?? CallToast.Build(ring, CallerName(ring), silent))
            {
                Tag = CallToast.Tag(ring.Room),
                Group = CallToast.Group,
                Priority = AppNotificationPriority.High,
                // The calls service stops a ring after 45 s; the toast never outlives it for long.
                Expiration = DateTimeOffset.Now + CallRingState.MaxRing,
            };
            AppNotificationManager.Default.Show(notification);
            if (notification.Id != 0) { rings.MarkToast(ring.Room); return; }
            DesktopLog.Write($"Windows did not accept the call alert; setting {AppNotificationManager.Default.Setting}");
        }
        catch (Exception ex) { DesktopLog.Error("Showing the call alert failed", ex); }
        // No toast (alerts off, Focus): bring the window back so the page rings in it.
        rings.MarkToast(ring.Room, false);
        ShowInbox();
    }

    private static async Task RemoveCallToastAsync(string room)
    {
        try { await AppNotificationManager.Default.RemoveByTagAndGroupAsync(CallToast.Tag(room), CallToast.Group); }
        catch (Exception ex) { DesktopLog.Error("Removing the call alert failed", ex); }
    }

    private static async Task RemoveAllCallToastsAsync()
    {
        try { await AppNotificationManager.Default.RemoveByGroupAsync(CallToast.Group); }
        catch (Exception) { /* Notifications unavailable; nothing to remove. */ }
    }

    // A ring whose end never arrives (page reload, lost connection) must not leave a toast behind.
    private void StartRingTimer()
    {
        if (ringTimer is null)
        {
            ringTimer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(5) };
            ringTimer.Tick += (_, _) =>
            {
                foreach (var room in rings.Expire(DateTimeOffset.UtcNow)) _ = RemoveCallToastAsync(room);
                if (pendingAnswer is { } answer && DateTimeOffset.UtcNow - answer.At > CallRingState.MaxRing) pendingAnswer = null;
                if (!rings.HasAny && pendingAnswer is null) ringTimer?.Stop();
            };
        }
        ringTimer.Start();
    }

    // Answer, Decline or the toast body, from App (a running app, or a cold start from a toast).
    public async Task HandleCallLaunchAsync(CallLaunch launch)
    {
        var now = DateTimeOffset.UtcNow;
        var token = launch.RingToken ?? rings.Get(launch.Room)?.RingToken;
        DesktopLog.Write($"Call alert: {launch.Action}{(token is null ? "" : " (ring token)")}");
        switch (launch.Action)
        {
            case CallLaunchAction.Answer:
                rings.Handle(launch.Room, now);
                _ = RemoveCallToastAsync(launch.Room);
                pendingAnswer = (launch.Room, token, now);
                StartRingTimer();
                ShowInbox(); // Starts the account on a cold start; the answer waits for the page.
                await AnswerPendingCallAsync();
                break;
            case CallLaunchAction.Decline:
                rings.Handle(launch.Room, now);
                _ = RemoveCallToastAsync(launch.Room);
                var declined = false;
                if (token is not null)
                {
                    try { declined = await CallRings.DeclineAsync(http, launch.Room, token, lifetime.Token); }
                    catch (Exception ex) when (ex is not OperationCanceledException) { DesktopLog.Error("Declining the call failed", ex); }
                    catch (OperationCanceledException) { }
                }
                if (!declined && callReady) await RunCallScriptAsync(CallBridge.DeclineScript(launch.Room, token));
                // Cold start from Decline: stay in the notification area rather than open a window.
                if (started is null) StartInNotificationArea();
                break;
            default:
                // The toast body. Windows dismissed the toast; while the ring lasts, put Answer and
                // Decline back in front of the person, then show the window.
                ShowInbox();
                if (rings.Get(launch.Room) is { } live && !rings.WasHandled(launch.Room, now)) ShowCallToast(live);
                break;
        }
    }

    // Called when the page is ready and configured, and directly when it already is.
    private async Task AnswerPendingCallAsync()
    {
        if (pendingAnswer is not { } answer || !callReady || !callsSignedIn) return;
        if (DateTimeOffset.UtcNow - answer.At > CallRingState.MaxRing) { pendingAnswer = null; return; }
        pendingAnswer = null;
        DesktopLog.Write("Answering the call");
        await RunCallScriptAsync(CallBridge.AnswerScript(answer.Room, answer.RingToken));
    }

    private async Task StopCallAlertsAsync()
    {
        pendingAnswer = null;
        rings.Clear();
        ringTimer?.Stop();
        await RemoveAllCallToastsAsync();
    }

    // ---- WNS: rings while the app is closed ------------------------------------------------------

    // After each sign-in and every start, and daily: Windows renews channels (they last 30 days).
    // Every failure is only logged: unpackaged and test builds have no channel, and a postbox
    // without WNS support refuses the registration; calls still ring while the app runs.
    private async Task RegisterCallPushAsync()
    {
#if UI_TESTS
        await Task.CompletedTask;
#else
        if (!Packaged) { DesktopLog.Write("Call push unavailable: not installed as a package"); return; }
        var identity = ViewModel.SelectedMailbox?.Address ?? ViewModel.Mailboxes.FirstOrDefault()?.Address;
        if (identity is null) return;
        try
        {
            var channel = await PushNotificationChannelManager.CreatePushNotificationChannelForApplicationAsync();
            if (!ReferenceEquals(channel, pushChannel))
            {
                if (pushChannel is not null) pushChannel.PushNotificationReceived -= PushChannel_Received;
                pushChannel = channel;
                channel.PushNotificationReceived += PushChannel_Received;
            }
            if (!WnsChannel.IsValid(channel.Uri)) { DesktopLog.Write("Call push channel refused: not a WNS address"); return; }
            var previous = ReadPushChannel();
            if (previous is not null && previous != channel.Uri)
            {
                try { await postbox.UnregisterDeviceAsync(previous, lifetime.Token); }
                catch (Exception ex) when (ex is not OperationCanceledException) { DesktopLog.Error("Removing the previous call push channel failed", ex); }
            }
            await postbox.RegisterDeviceAsync(identity, channel.Uri, "wns", "windows", lifetime.Token);
            SavePushChannel(channel.Uri);
            DesktopLog.Write($"Call push channel registered; expires {channel.ExpirationTime:yyyy-MM-dd}");
        }
        catch (OperationCanceledException) { }
        catch (PostboxException ex) { DesktopLog.Write($"Call push channel not registered: postbox {ex.StatusCode} {ex.Code}"); }
        catch (Exception ex) { DesktopLog.Error("Call push channel unavailable", ex); }
        if (pushTimer is null)
        {
            pushTimer = new DispatcherTimer { Interval = WnsChannel.RefreshInterval };
            pushTimer.Tick += async (_, _) => { if (callsSignedIn) await RegisterCallPushAsync(); };
            pushTimer.Start();
        }
#endif
    }

    // Sign-out: this PC stops ringing for the account.
    private async Task UnregisterCallPushAsync()
    {
        pushTimer?.Stop();
        pushTimer = null;
        if (pushChannel is { } channel)
        {
            channel.PushNotificationReceived -= PushChannel_Received;
            try { channel.Close(); } catch (Exception) { }
            pushChannel = null;
        }
        if (ReadPushChannel() is not { } registered) return;
        try { await postbox.UnregisterDeviceAsync(registered, lifetime.Token); }
        catch (Exception ex) when (ex is not OperationCanceledException) { DesktopLog.Error("Removing the call push channel failed", ex); }
        catch (OperationCanceledException) { }
        try { File.Delete(PushChannelFile); } catch (Exception ex) when (ex is IOException or UnauthorizedAccessException) { }
    }

    // A WNS call toast while the app runs: the app shows it itself, so it is not shown twice when the
    // page rings too, and its ring token is kept for Decline. Runs on a background thread; the
    // decision to cancel the system's toast must be made here, synchronously.
    private void PushChannel_Received(PushNotificationChannel sender, PushNotificationReceivedEventArgs e)
    {
        if (e.NotificationType != PushNotificationType.Toast || e.ToastNotification?.Content is not { } content) return;
        string payload;
        CallLaunch? launch = null;
        try
        {
            payload = content.GetXml();
            foreach (var node in content.GetElementsByTagName("action"))
                if (CallLaunch.Parse(node.Attributes?.GetNamedItem("arguments")?.InnerText) is { } parsed) { launch = parsed; break; }
            launch ??= CallLaunch.Parse(content.DocumentElement?.GetAttribute("launch"));
        }
        catch (Exception ex) { DesktopLog.Error("Reading a call push failed", ex); return; }
        if (launch is null) return;
        e.Cancel = true;
        var call = launch;
        DispatcherQueue.TryEnqueue(() =>
        {
            var now = DateTimeOffset.UtcNow;
            var ring = new CallRing(call.Room, call.Kind, null, null, null, null, call.RingToken);
            if (rings.WasHandled(call.Room, now)) return;
            if (!rings.Begin(ring, now)) return; // The page rang first; the ring token is merged in.
            StartRingTimer();
            DesktopLog.Write($"Incoming {call.Kind} call (push)");
            ShowCallToast(ring, payload);
        });
    }

    private static string? ReadPushChannel()
    {
        try { return File.Exists(PushChannelFile) && File.ReadAllText(PushChannelFile).Trim() is { } uri && WnsChannel.IsValid(uri) ? uri : null; }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException) { return null; }
    }

    private static void SavePushChannel(string uri)
    {
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(PushChannelFile)!);
            File.WriteAllText(PushChannelFile, uri);
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException) { DesktopLog.Error("Saving the call push channel failed", ex); }
    }
}
