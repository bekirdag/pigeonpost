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
//  idle it stays loaded, invisible and click-through, so an incoming call rings while the app runs.
//  With the window in front the page rings in it; otherwise the app shows a Windows incoming-call
//  alert instead (CallAlerts.cs).
public sealed partial class MainWindow
{
    private WebView2? callView;
    private CoreWebView2? callCore;
    private Task? callsStarting;
    private bool callReady;
    private bool callsSignedIn;
    private CallMode callMode = CallMode.Hidden;
    private bool callRinging;
    private (NativeCallSession Actor, string Peer, bool Video)? pendingCall;
    private NativeCallSession? callSession;
    private InboxViewModel? callInbox;
    private int callDocument;
    private sealed record CallPageTicket(WebView2 View, CoreWebView2 Core, NativeCallSession Actor, int Document);
    private int callRestarts;
    private Uri callPage = CallBridge.ProductionPage;

#if DEBUG || UI_TESTS
    // Local call tests only, never compiled into a Store build: PIGEONPOST_CALLS_PAGE (the harness's
    // native-call.html), _TOKEN and _MAILBOX (+ optional _HANDLE) for its stand-in postbox, _CALL
    // (peer:audio|video, placed once the page is ready) and _BROWSER_ARGS (fake media devices,
    // remote debugging for the test driver).
    private static string? CallsDebug(string name) =>
        Environment.GetEnvironmentVariable("PIGEONPOST_CALLS_" + name) is { Length: > 0 } value ? value : null;
    private sealed class FixtureCallTokens(string token) : IAccessTokenProvider
    {
        public Task<string> GetTokenAsync(CancellationToken ct) => Task.FromResult(token);
        public Task<string> RefreshTokenAsync(CancellationToken ct) => Task.FromResult(token);
    }
    private IAccessTokenProvider? fixtureCallTokens;
#endif

    private Mailbox? CurrentCallMailbox()
    {
#if DEBUG || UI_TESTS
        if (CallsDebug("TOKEN") is not null && CallsDebug("MAILBOX") is { } address) return new Mailbox(address, CallsDebug("HANDLE"));
#endif
        return ViewModel.SelectedMailbox;
    }
    private IAccessTokenProvider CallTokenProvider()
    {
#if DEBUG || UI_TESTS
        if (CallsDebug("TOKEN") is { } token) return fixtureCallTokens ??= new FixtureCallTokens(token);
#endif
#if UI_TESTS
        if (CallsDebug("PAGE") is null) return fixtureCallTokens ??= new FixtureCallTokens("ui-preview-only-call-token");
#endif
        return session;
    }
    private bool CallCurrent(NativeCallSession actor) => callsSignedIn && !lifetime.IsCancellationRequested
        && ReferenceEquals(callSession, actor) && ReferenceEquals(callInbox, ViewModel)
        && CurrentCallMailbox() is { } mailbox && actor.Matches(mailbox);
    private CallPageTicket? CaptureCallPage(bool requireReady = true)
    {
        if (requireReady && !callReady || callSession is not { } actor || !CallCurrent(actor)
            || callView is not { } view || callCore is not { } core) return null;
        var ticket = new CallPageTicket(view, core, actor, callDocument);
        return CallPageCurrent(ticket) ? ticket : null;
    }
    private bool CallPageCurrent(CallPageTicket ticket)
    {
        try
        {
            return CallCurrent(ticket.Actor) && ReferenceEquals(callView, ticket.View)
                && ReferenceEquals(callCore, ticket.Core) && callDocument == ticket.Document
                && CallBridge.IsPage(ParseUri(ticket.Core.Source), callPage);
        }
        catch (Exception ex) when (ex is COMException or InvalidOperationException) { return false; }
    }

    private static string CallsDataFolder => Path.Combine(
        Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Pigeonpost", "CallsWebView2");

    // Called once the inbox is open (and again after each mailbox switch, which only reconfigures).
    private Task StartCallsAsync()
    {
        if (lifetime.IsCancellationRequested || CurrentCallMailbox() is not { } mailbox) return Task.CompletedTask;
        if (callSession is { } current && CallCurrent(current))
        {
            if (callView is not null) return ConfigureCallsAsync();
            if (CallsStarting) return callsStarting!;
        }
        var replacing = callSession is not null;
        RemoveCallView();
        if (replacing) _ = StopCallAlertsAsync();
        NativeCallSession actor;
        try
        {
            var provider = CallTokenProvider();
            var fixture = false;
#if DEBUG || UI_TESTS
            fixture = provider is FixtureCallTokens;
            if (CallsDebug("PAGE") is { } supplied
                && (!fixture || !Uri.TryCreate(supplied, UriKind.Absolute, out var local)
                    || !local.IsLoopback || local.Scheme != Uri.UriSchemeHttp || !string.IsNullOrEmpty(local.UserInfo))) return Task.CompletedTask;
#endif
            actor = new NativeCallSession(provider, mailbox, allowUnboundFixture: fixture);
        }
        catch (AuthorizationRetiredException) { return Task.CompletedTask; }
        callSession = actor;
        callInbox = ViewModel;
        callsSignedIn = true;
        HookCallAlerts();
        UpdateCallButtons();
        return callsStarting = CreateCallViewAsync(actor);
    }

    private async Task CreateCallViewAsync(NativeCallSession actor)
    {
        WebView2? created = null;
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
            var environment = await CoreWebView2Environment.CreateWithOptionsAsync(null, CallsDataFolder,
                new CoreWebView2EnvironmentOptions { AdditionalBrowserArguments = arguments });
            if (!CallCurrent(actor)) return;
            var view = new WebView2
            {
                HorizontalAlignment = HorizontalAlignment.Left,
                VerticalAlignment = VerticalAlignment.Top,
            };
            created = view;
            AutomationProperties.SetName(view, "Call");
            AutomationProperties.SetAutomationId(view, "CallView");
            Grid.SetRow(view, 0);
            Root.Children.Add(view); // Last child: above the inbox, the viewer and the account bar.
            callView = view;
            ApplyCallMode(CallMode.Hidden);
            DesktopLog.Write($"Call view environment ready ({environment.BrowserVersionString})");
            await view.EnsureCoreWebView2Async(environment);
            if (!CallCurrent(actor) || !ReferenceEquals(callView, view)) { CloseCreatedCallView(view, actor); return; }
            var core = view.CoreWebView2;
            callCore = core;
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
            await core.AddScriptToExecuteOnDocumentCreatedAsync(CallBridge.RingObserverScript);
            if (!CallCurrent(actor) || !ReferenceEquals(callView, view)) { CloseCreatedCallView(view, actor); return; }
            // Retain the owned projection and bind callbacks to it, rather than comparing
            // repeatedly projected property/event-sender objects by managed identity.
            core.NavigationStarting += (_, e) => CallView_NavigationStarting(core, e);
            core.FrameNavigationStarting += (_, e) =>
            {
                if (!CallCurrent(actor) || !ReferenceEquals(callView, view) || !CallBridge.IsPage(ParseUri(e.Uri), callPage)) e.Cancel = true;
            };
            core.NewWindowRequested += (sender, e) =>
            {
                e.Handled = true;
                if (CallCurrent(actor) && ReferenceEquals(callView, view) && CallBridge.IsExternalLink(ParseUri(e.Uri))) _ = Launcher.LaunchUriAsync(new Uri(e.Uri));
            };
            core.PermissionRequested += (_, e) => CallView_PermissionRequested(core, e);
            core.ScreenCaptureStarting += (sender, e) =>
            {
                // Screen sharing is offered by the page itself; Windows' own picker chooses what is shared.
                var allowed = CallCurrent(actor) && ReferenceEquals(callView, view) && ReferenceEquals(callCore, core)
                    && CallBridge.IsPage(ParseUri(core.Source), callPage);
                e.Cancel = !allowed;
                DesktopLog.Write($"Call screen sharing {(allowed ? "started" : "refused")}");
            };
            core.DownloadStarting += (_, e) => { e.Cancel = true; e.Handled = true; };
            core.WebMessageReceived += (_, e) => CallView_WebMessageReceived(core, e);
            core.ProcessFailed += (_, e) => CallView_ProcessFailed(core, e);
            DesktopLog.Write("Call view created");
            core.Navigate(callPage.AbsoluteUri);
        }
        catch (Exception ex)
        {
            // Without WebView2 the inbox still works; the call buttons explain themselves.
            if (CallCurrent(actor)) DesktopLog.Error("Call view unavailable", ex);
            if (created is not null) CloseCreatedCallView(created, actor);
            else if (ReferenceEquals(callSession, actor)) RemoveCallView();
        }
    }

    private void CloseCreatedCallView(WebView2 view, NativeCallSession actor)
    {
        if (ReferenceEquals(callSession, actor) && ReferenceEquals(callView, view)) { RemoveCallView(); return; }
        Root.Children.Remove(view);
        try { view.Close(); } catch (Exception) { }
    }

    private bool CallsStarting => callsStarting is { IsCompleted: false };

    private static Uri? ParseUri(string? value) => Uri.TryCreate(value, UriKind.Absolute, out var uri) ? uri : null;

    // The page, and nothing else. A link out (the meeting link's own page, a help link) opens in the
    // browser rather than replacing the call client.
    private void CallView_NavigationStarting(CoreWebView2 sender, CoreWebView2NavigationStartingEventArgs e)
    {
        if (callSession is not { } actor || !CallCurrent(actor) || !ReferenceEquals(callCore, sender))
        {
            e.Cancel = true;
            DesktopLog.Write("Call navigation refused: retired host");
            return;
        }
        var target = ParseUri(e.Uri);
        if (CallBridge.IsPage(target, callPage))
        {
            if (callDocument > 0) pendingCall = null;
            callDocument++;
            callReady = false; // A reload starts again from a page that knows nothing.
            DesktopLog.Write("Call page navigating");
            return;
        }
        e.Cancel = true;
        if (e.IsUserInitiated && CallBridge.IsExternalLink(target)) _ = Launcher.LaunchUriAsync(target!);
    }

    // The microphone and camera are for the call page only. Windows' privacy settings still apply.
    private void CallView_PermissionRequested(CoreWebView2 sender, CoreWebView2PermissionRequestedEventArgs e)
    {
        // e.Uri is the requesting origin, not its document URL (Microsoft WebView2 API).
        var allowed = CaptureCallPage(requireReady: false) is { } page && ReferenceEquals(page.Core, sender)
            && CallBridge.SameOrigin(ParseUri(e.Uri), callPage) && e.PermissionKind is (CoreWebView2PermissionKind.Microphone
            or CoreWebView2PermissionKind.Camera or CoreWebView2PermissionKind.Autoplay);
        e.State = allowed ? CoreWebView2PermissionState.Allow : CoreWebView2PermissionState.Deny;
        e.SavesInProfile = false; // This handler decides every time.
        DesktopLog.Write($"Call permission {e.PermissionKind}: {(allowed ? "allowed" : "denied")}");
    }

    private async void CallView_WebMessageReceived(CoreWebView2 sender, CoreWebView2WebMessageReceivedEventArgs e)
    {
        if (CaptureCallPage(requireReady: false) is not { } page || !ReferenceEquals(page.Core, sender)
            || !CallBridge.IsPage(ParseUri(e.Source), callPage))
        {
            DesktopLog.Write("Call bridge ignored: host or document changed");
            return;
        }
        string? json;
        try { json = e.WebMessageAsJson; } catch (Exception) { return; }
        switch (CallBridge.Parse(json))
        {
            case { Type: "ready" }:
                callReady = true;
                callRestarts = 0;
                DesktopLog.Write("Call page ready");
                await ConfigureCallsAsync();
                if (!CallPageCurrent(page)) return;
                await AnswerPendingCallAsync();
                if (!CallPageCurrent(page)) return;
                if (pendingCall is { } call && ReferenceEquals(call.Actor, page.Actor)) { pendingCall = null; await PlaceCallAsync(call.Peer, call.Video); }
#if DEBUG || UI_TESTS
                if (CallsDebug("CALL") is { } auto && auto.LastIndexOf(':') is > 0 and var colon)
                {
                    await Task.Delay(1500);
                    if (!CallPageCurrent(page)) return;
                    await PlaceCallAsync(auto[..colon], auto[(colon + 1)..] == "video");
                }
#endif
                break;
            case { Type: "mode", Mode: var mode }:
                ApplyCallMode(mode);
                break;
            case { Type: "ring_card", Ringing: var ringing }:
                callRinging = ringing;
                ApplyCallMode(callMode);
                break;
            case { Type: "ring", Ring: { } ring }:
                CallPage_Ring(ring);
                break;
            case { Type: "ring_ended", Room: { } ended }:
                CallPage_RingEnded(ended);
                break;
            case { Type: "call", State: var state, Room: var room }:
                CallPage_Call(state, room);
                break;
            case { Type: "renew" }:
                await RenewCallTokenAsync(page);
                break;
            case { Type: "toast", Text: { } text }:
                AccountStatus.Text = text;
                break;
        }
    }

    private void CallView_ProcessFailed(CoreWebView2 sender, CoreWebView2ProcessFailedEventArgs e)
    {
        if (callSession is not { } actor || !CallCurrent(actor) || !ReferenceEquals(callCore, sender)) return;
        DesktopLog.Write($"Call view process failed: {e.ProcessFailedKind}");
        callReady = false;
        callRinging = false;
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

    // Tell the page who is signed in, and the names of the people in this mailbox.
    private async Task ConfigureCallsAsync()
    {
        if (!callsSignedIn) return;
        if (CurrentCallMailbox() is null) { RemoveCallView(); return; }
        if (callSession is not { } actor || !CallCurrent(actor)) { await StartCallsAsync(); return; }
        if (CaptureCallPage() is not { } page) return;
        var mailbox = CurrentCallMailbox()!;
        var conversations = ViewModel.AllConversations.ToArray();
        var nativeRing = WantNativeRing;
        string? token = null;
        try { token = await actor.TokenAsync(false, lifetime.Token); }
        catch (OperationCanceledException) { return; }
        catch (Exception ex) { if (CallPageCurrent(page)) DesktopLog.Error("Call token unavailable", ex); }
        if (!CallPageCurrent(page)) return;
        if (await RunCallScriptAsync(page, CallBridge.ConfigureScript(token, mailbox, conversations, nativeRing)) && CallPageCurrent(page)) configuredNativeRing = nativeRing;
    }

    private async Task RenewCallTokenAsync(CallPageTicket page)
    {
        string? fresh = null;
        try { fresh = await page.Actor.TokenAsync(true, lifetime.Token); }
        catch (Exception ex) when (ex is not OperationCanceledException) { if (CallPageCurrent(page)) DesktopLog.Error("Call token renewal failed", ex); }
        catch (OperationCanceledException) { return; }
        if (CallPageCurrent(page)) await RunCallScriptAsync(page, CallBridge.RenewedScript(fresh));
    }

    private async Task<bool> RunCallScriptAsync(CallPageTicket page, string script)
    {
        if (!CallPageCurrent(page)) return false;
        try { await page.Core.ExecuteScriptAsync(script); return CallPageCurrent(page); }
        catch (Exception ex) { if (CallPageCurrent(page)) DesktopLog.Error("Call page script failed", ex); return false; }
    }

    private async Task PlaceCallAsync(string peer, bool video)
    {
#if UI_TESTS
        if (CallsDebug("PAGE") is null) { AccountStatus.Text = "Calling is not configured in this preview."; return; }
#endif
        if (!callsSignedIn) return;
        if (callSession is not { } actor || !CallCurrent(actor))
        {
            await StartCallsAsync();
            return; // A different login/inbox needs a new explicit call action.
        }
        if (!callReady)
        {
            pendingCall = (actor, peer, video);
            AccountStatus.Text = "Connecting the call…";
            if (callView is null && !CallsStarting && callsSignedIn) await StartCallsAsync();
            if (!CallCurrent(actor)) return;
            if (callView is null && !CallsStarting) { pendingCall = null; AccountStatus.Text = "Calls aren't available right now. Please try again."; }
            return;
        }
        DesktopLog.Write($"Placing a {(video ? "video" : "voice")} call");
        if (CaptureCallPage() is { } page) await RunCallScriptAsync(page, CallBridge.CallScript(peer, video));
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
        var enabled = callSession is { } actor && CallCurrent(actor) && callMode == CallMode.Hidden && ViewModel.SelectedConversation is { IsMine: false };
        VoiceCallButton.IsEnabled = VideoCallButton.IsEnabled = enabled;
    }

    // Full: the call covers the window (while it only rings, just the ring card in the top-right
    // corner, over the inbox). Mini: a strip at the top, with the inbox moved down. Hidden: loaded
    // but invisible, one pixel, out of the way of clicks and screen readers.
    private void ApplyCallMode(CallMode mode)
    {
        var previous = callMode;
        callMode = mode;
        if (callView is { } view)
        {
            var shown = mode != CallMode.Hidden;
            CallRow.Height = new GridLength(mode == CallMode.Mini ? 64 : 0);
            Grid.SetRowSpan(view, mode == CallMode.Mini ? 1 : 4);
            var ringCard = mode == CallMode.Full && callRinging;
            view.Width = ringCard ? 424 : shown ? double.NaN : 1;
            view.Height = ringCard ? 116 : shown ? double.NaN : 1;
            view.HorizontalAlignment = ringCard ? HorizontalAlignment.Right : shown ? HorizontalAlignment.Stretch : HorizontalAlignment.Left;
            view.VerticalAlignment = shown && !ringCard ? VerticalAlignment.Stretch : VerticalAlignment.Top;
            if (Root.Background is Microsoft.UI.Xaml.Media.SolidColorBrush ground) view.DefaultBackgroundColor = ground.Color;
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
            if (ringCard != lastRingCard) { lastRingCard = ringCard; DesktopLog.Write(ringCard ? "Call view ring card" : "Call view ring card closed"); }
        }
        UpdateCallButtons();
    }

    private bool lastRingCard;

    // Sign-out retires this host; the next sign-in receives a fresh call page.
    private static async Task ShutdownCallPageAsync(CoreWebView2 core)
    {
        await core.ExecuteScriptAsync(CallBridge.SignOutScript);
    }

    private async Task StopCallsAsync()
    {
        var page = CaptureCallPage(requireReady: false);
        var view = DetachCallView();
        var alerts = StopCallAlertsAsync();
        try
        {
            // Only this retired view receives shutdown; no token or user action is dispatched.
            if (page is not null && CallBridge.IsPage(ParseUri(page.Core.Source), callPage))
                await ShutdownCallPageAsync(page.Core).WaitAsync(TimeSpan.FromSeconds(2));
        }
        catch (Exception) { }
        finally { try { view?.Close(); } catch (Exception) { } }
        await alerts;
        if (callSession is null) ApplyCallMode(CallMode.Hidden);
    }

    private void RemoveCallView()
    {
        var view = DetachCallView();
        try { view?.Close(); } catch (Exception) { }
    }

    private WebView2? DetachCallView()
    {
        callSession?.Dispose();
        callSession = null;
        callInbox = null;
        callsSignedIn = false;
        pendingCall = null;
        callDocument = 0;
        var view = callView;
        callView = null;
        callCore = null;
        callReady = false;
        callRinging = false;
        callMode = CallMode.Hidden;
        KeepDisplayAwake(false);
        if (view is not null) Root.Children.Remove(view);
        CallRow.Height = new GridLength(0);
        UpdateCallButtons();
        return view;
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
