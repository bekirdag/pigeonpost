using System.Runtime.InteropServices;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.Web.WebView2.Core;
using Pigeonpost.Core;
using Windows.System;

namespace Pigeonpost.Desktop;

//  Voice and video calls.
//
//  The calls are the web inbox's own (site-inbox/calls.js), run in an embedded WebView2 rather than
//  rewritten: the same client rings, answers, runs group calls and meeting links, chat, device
//  choice, noise removal and screen sharing. WebView2 (Chromium) carries the media, encrypted end to
//  end (DTLS-SRTP) exactly as in Edge.
//
//  The app owns sign-in. The page (native-call.html + native-call.js) is handed the member token and
//  the mailbox in use, asks the app when the token needs renewing, and says how much of the window
//  the call needs: all of it, a 64px strip while the person reads mail during a call, or none. While
//  idle it stays loaded, invisible and click-through, so an incoming call rings while the app runs
//  (including from the notification area: a ring brings the window back).
public sealed partial class MainWindow
{
    private WebView2? callView;
    private Task? callsStarting;
    private bool callReady;
    private bool callsSignedIn;
    private CallMode callMode = CallMode.Hidden;
    private (string Peer, bool Video)? pendingCall;
    private int callRestarts;
    private Uri callPage = CallBridge.ProductionPage;

#if DEBUG || UI_TESTS
    // Local call tests only, never compiled into a Store build: PIGEONPOST_CALLS_PAGE (the harness's
    // native-call.html), _TOKEN and _MAILBOX (+ optional _HANDLE) for its stand-in postbox, _CALL
    // (peer:audio|video, placed once the page is ready) and _BROWSER_ARGS (fake media devices,
    // remote debugging for the test driver).
    private static string? CallsDebug(string name) =>
        Environment.GetEnvironmentVariable("PIGEONPOST_CALLS_" + name) is { Length: > 0 } value ? value : null;
#endif

    private static string CallsDataFolder => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Pigeonpost", "CallsWebView2");

    // Called once the inbox is open (and again after each mailbox switch, which only reconfigures).
    private Task StartCallsAsync()
    {
        callsSignedIn = true;
        UpdateCallButtons();
        if (callView is not null) return ConfigureCallsAsync();
        if (CallsStarting) return callsStarting!;
        return callsStarting = CreateCallViewAsync();
    }

    private async Task CreateCallViewAsync()
    {
        try
        {
#if UI_TESTS
            // Fixture builds have no account; they only run calls against the local harness.
            if (CallsDebug("PAGE") is null) return;
#endif
#if DEBUG || UI_TESTS
            if (CallsDebug("PAGE") is { } page && Uri.TryCreate(page, UriKind.Absolute, out var harness)) callPage = harness;
#endif
            // A ring must sound without a click in the page, as the phone apps' web views allow.
            var arguments = "--autoplay-policy=no-user-gesture-required";
#if DEBUG || UI_TESTS
            if (CallsDebug("BROWSER_ARGS") is { } extra) arguments += " " + extra;
#endif
            Directory.CreateDirectory(CallsDataFolder);
            DesktopLog.Write("Call view starting");
            // The page is see-through except for the call itself (a ring floats over the inbox), so
            // the web view must not paint its own white background: set before the browser starts.
            Environment.SetEnvironmentVariable("WEBVIEW2_DEFAULT_BACKGROUND_COLOR", "0");
            var environment = await CoreWebView2Environment.CreateWithOptionsAsync(null, CallsDataFolder,
                new CoreWebView2EnvironmentOptions { AdditionalBrowserArguments = arguments });
            var view = new WebView2
            {
                DefaultBackgroundColor = Microsoft.UI.Colors.Transparent,
                HorizontalAlignment = HorizontalAlignment.Left,
                VerticalAlignment = VerticalAlignment.Top,
            };
            AutomationProperties.SetName(view, "Call");
            AutomationProperties.SetAutomationId(view, "CallView");
            Grid.SetRow(view, 0);
            Root.Children.Add(view); // Last child: above the inbox, the viewer and the account bar.
            callView = view;
            ApplyCallMode(CallMode.Hidden);
            DesktopLog.Write($"Call view environment ready ({environment.BrowserVersionString})");
            await view.EnsureCoreWebView2Async(environment);
            view.DefaultBackgroundColor = Microsoft.UI.Colors.Transparent;
            var core = view.CoreWebView2;
            var settings = core.Settings;
            settings.AreDefaultContextMenusEnabled = false;
            settings.AreBrowserAcceleratorKeysEnabled = false;
            settings.IsZoomControlEnabled = false;
            settings.IsStatusBarEnabled = false;
            settings.IsGeneralAutofillEnabled = false;
            settings.IsPasswordAutosaveEnabled = false;
#if DEBUG || UI_TESTS
            settings.AreDevToolsEnabled = true;
#else
            settings.AreDevToolsEnabled = false;
#endif
            core.NavigationStarting += CallView_NavigationStarting;
            core.FrameNavigationStarting += (_, e) =>
            {
                if (!CallBridge.SameOrigin(ParseUri(e.Uri), callPage)) e.Cancel = true;
            };
            core.NewWindowRequested += (sender, e) =>
            {
                e.Handled = true;
                if (CallBridge.IsExternalLink(ParseUri(e.Uri))) _ = Launcher.LaunchUriAsync(new Uri(e.Uri));
            };
            core.PermissionRequested += CallView_PermissionRequested;
            core.ScreenCaptureStarting += (sender, e) =>
            {
                // Screen sharing is offered by the page itself; Windows' own picker chooses what is shared.
                var allowed = CallBridge.IsPage(ParseUri(sender.Source), callPage);
                e.Cancel = !allowed;
                DesktopLog.Write($"Call screen sharing {(allowed ? "started" : "refused")}");
            };
            core.DownloadStarting += (_, e) => { e.Cancel = true; e.Handled = true; };
            core.WebMessageReceived += CallView_WebMessageReceived;
            core.ProcessFailed += CallView_ProcessFailed;
            DesktopLog.Write("Call view created");
            core.Navigate(callPage.AbsoluteUri);
        }
        catch (Exception ex)
        {
            // Without WebView2 the inbox still works; the call buttons explain themselves.
            DesktopLog.Error("Call view unavailable", ex);
            RemoveCallView();
        }
    }

    private bool CallsStarting => callsStarting is { IsCompleted: false };

    private static Uri? ParseUri(string? value) => Uri.TryCreate(value, UriKind.Absolute, out var uri) ? uri : null;

    // The page, and nothing else. A link out (the meeting link's own page, a help link) opens in the
    // browser rather than replacing the call client.
    private void CallView_NavigationStarting(CoreWebView2 sender, CoreWebView2NavigationStartingEventArgs e)
    {
        var target = ParseUri(e.Uri);
        if (CallBridge.IsPage(target, callPage))
        {
            callReady = false; // A reload starts again from a page that knows nothing.
            return;
        }
        e.Cancel = true;
        if (e.IsUserInitiated && CallBridge.IsExternalLink(target)) _ = Launcher.LaunchUriAsync(target!);
    }

    // The microphone and camera are for the call page only. Windows' privacy settings still apply.
    private void CallView_PermissionRequested(CoreWebView2 sender, CoreWebView2PermissionRequestedEventArgs e)
    {
        var allowed = CallBridge.SameOrigin(ParseUri(e.Uri), callPage) && e.PermissionKind is CoreWebView2PermissionKind.Microphone
            or CoreWebView2PermissionKind.Camera or CoreWebView2PermissionKind.Autoplay;
        e.State = allowed ? CoreWebView2PermissionState.Allow : CoreWebView2PermissionState.Deny;
        e.SavesInProfile = false; // This handler decides every time.
        DesktopLog.Write($"Call permission {e.PermissionKind}: {(allowed ? "allowed" : "denied")}");
    }

    private async void CallView_WebMessageReceived(CoreWebView2 sender, CoreWebView2WebMessageReceivedEventArgs e)
    {
        if (!CallBridge.IsPage(ParseUri(e.Source), callPage)) return;
        string? json;
        try { json = e.WebMessageAsJson; } catch (Exception) { return; }
        switch (CallBridge.Parse(json))
        {
            case { Type: "ready" }:
                callReady = true;
                callRestarts = 0;
                DesktopLog.Write("Call page ready");
                await ConfigureCallsAsync();
                if (pendingCall is { } call) { pendingCall = null; await PlaceCallAsync(call.Peer, call.Video); }
#if DEBUG || UI_TESTS
                if (CallsDebug("CALL") is { } auto && auto.LastIndexOf(':') is > 0 and var colon)
                {
                    await Task.Delay(1500);
                    await PlaceCallAsync(auto[..colon], auto[(colon + 1)..] == "video");
                }
#endif
                break;
            case { Type: "mode", Mode: var mode }:
                ApplyCallMode(mode);
                break;
            case { Type: "renew" }:
                await RenewCallTokenAsync();
                break;
            case { Type: "toast", Text: { } text }:
                AccountStatus.Text = text;
                break;
        }
    }

    private void CallView_ProcessFailed(CoreWebView2 sender, CoreWebView2ProcessFailedEventArgs e)
    {
        DesktopLog.Write($"Call view process failed: {e.ProcessFailedKind}");
        callReady = false;
        ApplyCallMode(CallMode.Hidden);
        if (lifetime.IsCancellationRequested || !callsSignedIn || ++callRestarts > 3) return;
        if (e.ProcessFailedKind == CoreWebView2ProcessFailedKind.BrowserProcessExited)
        {
            RemoveCallView();
            _ = StartCallsAsync();
        }
        else
        {
            try { sender.Navigate(callPage.AbsoluteUri); } catch (Exception ex) { DesktopLog.Error("Call view reload failed", ex); }
        }
    }

    private async Task<(string? Token, Mailbox Mailbox)?> CallIdentityAsync()
    {
#if DEBUG || UI_TESTS
        if (CallsDebug("TOKEN") is { } token && CallsDebug("MAILBOX") is { } address) return (token, new Mailbox(address, CallsDebug("HANDLE")));
#endif
        if (ViewModel.SelectedMailbox is not { } mailbox) return null;
        try { return (await session.GetTokenAsync(lifetime.Token), mailbox); }
        catch (OperationCanceledException) { return null; }
        catch (Exception ex) { DesktopLog.Error("Call token unavailable", ex); return (null, mailbox); }
    }

    // Tell the page who is signed in, and the names of the people in this mailbox.
    private async Task ConfigureCallsAsync()
    {
        if (!callReady || !callsSignedIn || await CallIdentityAsync() is not { } identity) return;
        await RunCallScriptAsync(CallBridge.ConfigureScript(identity.Token, identity.Mailbox, ViewModel.AllConversations));
    }

    private async Task RenewCallTokenAsync()
    {
        string? fresh = null;
#if DEBUG || UI_TESTS
        if (CallsDebug("TOKEN") is { } token) fresh = token;
        else
#endif
        try { fresh = await session.RefreshTokenAsync(lifetime.Token); }
        catch (Exception ex) when (ex is not OperationCanceledException) { DesktopLog.Error("Call token renewal failed", ex); }
        catch (OperationCanceledException) { return; }
        await RunCallScriptAsync(CallBridge.RenewedScript(fresh));
    }

    private async Task RunCallScriptAsync(string script)
    {
        if (callView?.CoreWebView2 is not { } core) return;
        try { await core.ExecuteScriptAsync(script); }
        catch (Exception ex) { DesktopLog.Error("Call page script failed", ex); }
    }

    private async Task PlaceCallAsync(string peer, bool video)
    {
        if (!callReady)
        {
            pendingCall = (peer, video);
            AccountStatus.Text = "Connecting the call…";
            if (callView is null && !CallsStarting && callsSignedIn) await StartCallsAsync();
            if (callView is null && !CallsStarting) { pendingCall = null; AccountStatus.Text = "Calls aren't available right now. Please try again."; }
            return;
        }
        DesktopLog.Write($"Placing a {(video ? "video" : "voice")} call");
        await RunCallScriptAsync(CallBridge.CallScript(peer, video));
    }

    private async void VoiceCall_Click(object sender, RoutedEventArgs e)
    {
        if (ViewModel.SelectedConversation is { } conversation) await PlaceCallAsync(conversation.Peer, video: false);
    }

    private async void VideoCall_Click(object sender, RoutedEventArgs e)
    {
        if (ViewModel.SelectedConversation is { } conversation) await PlaceCallAsync(conversation.Peer, video: true);
    }

    private void UpdateCallButtons()
    {
        var enabled = callsSignedIn && callMode == CallMode.Hidden && ViewModel.SelectedConversation is { IsMine: false };
        VoiceCallButton.IsEnabled = VideoCallButton.IsEnabled = enabled;
    }

    // Full: the call covers the window. Mini: a strip at the top, with the inbox moved down. Hidden:
    // loaded but invisible, one pixel, out of the way of clicks and screen readers.
    private void ApplyCallMode(CallMode mode)
    {
        var previous = callMode;
        callMode = mode;
        if (callView is { } view)
        {
            var shown = mode != CallMode.Hidden;
            CallRow.Height = new GridLength(mode == CallMode.Mini ? 64 : 0);
            Grid.SetRowSpan(view, mode == CallMode.Mini ? 1 : 4);
            view.Width = view.Height = shown ? double.NaN : 1;
            view.HorizontalAlignment = shown ? HorizontalAlignment.Stretch : HorizontalAlignment.Left;
            view.VerticalAlignment = shown ? VerticalAlignment.Stretch : VerticalAlignment.Top;
            view.Opacity = shown ? 1 : 0;
            view.IsHitTestVisible = shown;
            view.IsTabStop = shown;
            AutomationProperties.SetAccessibilityView(view, shown ? AccessibilityView.Content : AccessibilityView.Raw);
            KeepDisplayAwake(shown);
            if (mode == CallMode.Full && previous == CallMode.Hidden)
            {
                // An incoming ring while the window is hidden in the notification area or minimised.
                if (!AppWindow.IsVisible || AppWindow.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Minimized }) ShowInbox();
                view.Focus(FocusState.Programmatic);
            }
            if (previous != mode) DesktopLog.Write($"Call view {mode}");
        }
        UpdateCallButtons();
    }

    // Sign-out ends any call and forgets the member; the page stays loaded for the next sign-in.
    private async Task StopCallsAsync()
    {
        callsSignedIn = false;
        pendingCall = null;
        await RunCallScriptAsync(CallBridge.SignOutScript);
        ApplyCallMode(CallMode.Hidden);
    }

    private void RemoveCallView()
    {
        if (callView is not { } view) return;
        callView = null;
        callReady = false;
        KeepDisplayAwake(false);
        Root.Children.Remove(view);
        CallRow.Height = new GridLength(0);
        try { view.Close(); } catch (Exception) { }
        UpdateCallButtons();
    }

    // A call keeps the display awake, as a phone call does.
    private bool displayHeld;
    private void KeepDisplayAwake(bool hold)
    {
        if (hold == displayHeld) return;
        displayHeld = hold;
        const uint Continuous = 0x80000000, SystemRequired = 0x00000001, DisplayRequired = 0x00000002;
        _ = SetThreadExecutionState(hold ? Continuous | SystemRequired | DisplayRequired : Continuous);
    }

    [DllImport("kernel32.dll")]
    private static extern uint SetThreadExecutionState(uint flags);
}
