using System.Diagnostics;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Pigeonpost.Core;
using Pigeonpost.Core.Groups;

namespace Pigeonpost.Desktop;

//  Phase 6 on Windows: live streams in groups (docs/streaming.md). The group shows a red LIVE
//  banner with Watch, Go live for members the group's policy allows, and stage invitations. The
//  stream itself (end-to-end encrypted frames, viewer count, reactions, the stage) is the web
//  client's own live.js, run in the WebView2 call host through PigeonpostNative.watchStream/goLive.
public sealed partial class MainWindow
{
    private Border? groupLiveBanner;
    private TextBlock? groupLiveText;
    private Button? groupLiveWatch, groupGoLive;
    private InfoBar? groupStage;

    private sealed class StreamAttempt(NativeCallSession actor, string groupId)
    {
        internal readonly Guid Id = Guid.NewGuid();
        internal readonly NativeCallSession Actor = actor;
        internal readonly string GroupId = groupId;
        internal readonly TaskCompletionSource<CallPageMessage> Result = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal CallPageTicket? Page;
    }
    private StreamAttempt? streamAttempt;
    // The stream the call host shows for this login, with the stage state last handed to it.
    private sealed record WatchedStream(NativeCallSession Actor, CallPageTicket Page, string GroupId, string StreamId, string? Stage);
    private WatchedStream? watchedStream;
    private Func<string, Task<GroupStream?>>? streamStatusSource;

    private void BuildLiveControls(StackPanel banners)
    {
        var grid = new Grid { ColumnSpacing = 10, Padding = new Thickness(12, 8, 8, 8) };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var badge = new Border
        {
            Background = new SolidColorBrush(Microsoft.UI.ColorHelper.FromArgb(0xFF, 0xD7, 0x26, 0x3D)), CornerRadius = new CornerRadius(4),
            Padding = new Thickness(6, 2, 6, 2), VerticalAlignment = VerticalAlignment.Center,
            Child = new TextBlock { Text = "LIVE", FontSize = 11, FontWeight = Microsoft.UI.Text.FontWeights.Bold, Foreground = new SolidColorBrush(Microsoft.UI.Colors.White) },
        };
        AutomationProperties.SetAccessibilityView(badge, Microsoft.UI.Xaml.Automation.Peers.AccessibilityView.Raw);
        grid.Children.Add(badge);
        groupLiveText = new TextBlock { VerticalAlignment = VerticalAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold };
        Grid.SetColumn(groupLiveText, 1);
        grid.Children.Add(groupLiveText);
        groupLiveWatch = new Button { Content = "Watch", Style = (Style)Application.Current.Resources["AccentButtonStyle"] };
        AutomationProperties.SetName(groupLiveWatch, "Watch the live stream");
        groupLiveWatch.Click += async (_, _) => await WatchLiveAsync();
        Grid.SetColumn(groupLiveWatch, 2);
        grid.Children.Add(groupLiveWatch);
        groupLiveBanner = new Border
        {
            Child = grid, CornerRadius = new CornerRadius(8), BorderThickness = new Thickness(1), Visibility = Visibility.Collapsed,
            BorderBrush = new SolidColorBrush(Microsoft.UI.ColorHelper.FromArgb(0x99, 0xD7, 0x26, 0x3D)),
            Background = new SolidColorBrush(Microsoft.UI.ColorHelper.FromArgb(0x1F, 0xD7, 0x26, 0x3D)),
        };
        AutomationProperties.SetName(groupLiveBanner, "Live stream");
        banners.Children.Insert(0, groupLiveBanner);

        groupStage = new InfoBar { IsClosable = false, Severity = InfoBarSeverity.Informational, Title = "Stage invitation" };
        var open = new Button { Content = "Open the stream" };
        AutomationProperties.SetName(open, "Open the stream to answer the stage invitation");
        open.Click += async (_, _) => await WatchLiveAsync();
        groupStage.ActionButton = open;
        AutomationProperties.SetName(groupStage, "Stage invitation");
        banners.Children.Insert(1, groupStage);
    }

    private Button BuildGoLiveButton()
    {
        groupGoLive = new Button { Content = new FontIcon { Glyph = "", FontSize = 14 }, Style = (Style)Root.Resources["ToolbarIconButton"], Visibility = Visibility.Collapsed };
        AutomationProperties.SetName(groupGoLive, "Go live");
        ToolTipService.SetToolTip(groupGoLive, "Go live");
        groupGoLive.Click += async (_, _) => await GoLiveAsync();
        return groupGoLive;
    }

    private void UpdateLiveControls()
    {
        if (groupLiveBanner is null || groupLiveText is null || groupLiveWatch is null || groupStage is null) return;
        var m = groupsModel;
        var live = m.LiveStream;
        groupLiveBanner.Visibility = ToVisibility(live is not null);
        groupLiveText.Text = m.LiveBannerText;
        var watching = live is not null && watchedStream is { } w && w.StreamId == live.Id && WatchCurrent(w);
        groupLiveWatch.Content = watching ? "Return" : m.LiveIsMine ? "Open" : "Watch";
        AutomationProperties.SetName(groupLiveWatch, watching ? "Return to the live stream" : m.LiveIsMine ? "Open your live stream" : "Watch the live stream");
        AutomationProperties.SetHelpText(groupLiveBanner, m.LiveBannerText);
        var invite = watching ? null : m.StageInviteText;
        groupStage.IsOpen = invite is not null;
        groupStage.Message = invite ?? "";
        if (groupGoLive is not null) groupGoLive.Visibility = ToVisibility(m.ShowsGoLive);
    }

    // The banner and Go live follow the selected group's stream, read at most every 10 seconds
    // (and at once when another group is selected).
    private (string? Group, DateTimeOffset At) lastStreamCheck;
    private async Task UpdateGroupStreamAsync(bool force = false)
    {
        var gid = groupsModel.Selected?.Id;
        if (!groupsModel.Streaming || gid is null) { UpdateLiveControls(); return; }
        if (!force && lastStreamCheck.Group == gid && DateTimeOffset.UtcNow - lastStreamCheck.At < TimeSpan.FromSeconds(10)) return;
        lastStreamCheck = (gid, DateTimeOffset.UtcNow);
        await groupsModel.RefreshStreamAsync();
        UpdateLiveControls();
    }

    private async Task WatchLiveAsync()
    {
        if (groupsModel.Info is not { IsActive: true } info || groupsModel.LiveStream is not { } live) return;
        await StartStreamAsync(info.Id, info.DisplayName, live.Id, null);
    }

    private async Task GoLiveAsync()
    {
        if (dialogOpen || groupsModel.Info is not { IsActive: true } info || !groupsModel.ShowsGoLive) return;
        dialogOpen = true;
        (string Title, bool Camera, bool Screen)? choice = null;
        try
        {
            var title = new TextBox { Header = "Title", PlaceholderText = "Sprint demo", MaxLength = 100 };
            AutomationProperties.SetName(title, "Stream title");
            var camera = new CheckBox { Content = "Camera", IsChecked = true };
            AutomationProperties.SetName(camera, "Show my camera");
            var screen = new CheckBox { Content = "Screen", IsChecked = false };
            AutomationProperties.SetName(screen, "Share my screen");
            var problem = new TextBlock { Foreground = new SolidColorBrush(Microsoft.UI.ColorHelper.FromArgb(0xFF, 0xC4, 0x2B, 0x1C)), TextWrapping = TextWrapping.Wrap, Visibility = Visibility.Collapsed };
            var panel = new StackPanel { Spacing = 12, MinWidth = 360, MaxWidth = 460 };
            panel.Children.Add(title);
            panel.Children.Add(new TextBlock { Text = "Show", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
            panel.Children.Add(camera);
            panel.Children.Add(screen);
            panel.Children.Add(new TextBlock { Text = "Everyone in this group can watch. The stream is end-to-end encrypted and isn't recorded; it can run for up to 4 hours.", TextWrapping = TextWrapping.Wrap, FontSize = 12, Opacity = 0.8 });
            panel.Children.Add(problem);
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = "Go live in " + info.DisplayName,
                Content = panel, PrimaryButtonText = "Go live", CloseButtonText = "Cancel", IsPrimaryButtonEnabled = false,
            };
            void Validate()
            {
                var titled = GroupStreams.ValidTitle(title.Text) is not null;
                var source = camera.IsChecked == true || screen.IsChecked == true;
                dialog.IsPrimaryButtonEnabled = titled && source;
                problem.Text = !titled ? "Give your stream a title." : !source ? "Choose camera, screen or both." : "";
                problem.Visibility = ToVisibility(title.Text.Length > 0 && problem.Text.Length > 0);
            }
            title.TextChanged += (_, _) => Validate();
            camera.Click += (_, _) => Validate();
            screen.Click += (_, _) => Validate();
            if (await dialog.ShowAsync() != ContentDialogResult.Primary) return;
            if (GroupStreams.ValidTitle(title.Text) is { } clean) choice = (clean, camera.IsChecked == true, screen.IsChecked == true);
        }
        finally { dialogOpen = false; }
        if (choice is { } c && groupsModel.Info?.Id == info.Id) await StartStreamAsync(info.Id, info.DisplayName, null, c);
    }

    // Watch (streamId) or go live (options) through the call host, as the inbox shown in it. The
    // answer is accepted only for this operation and page document, and only while this login and
    // inbox are still the ones that asked.
    private async Task StartStreamAsync(string groupId, string groupName, string? streamId, (string Title, bool Camera, bool Screen)? goLive, string? asMailbox = null)
    {
        var expected = asMailbox ?? groupsModel.Mailbox;
#if UI_TESTS
        if (CallsDebug("PAGE") is null)
        {
            DesktopLog.Write(goLive is { } g ? $"Stream go live requested ({(g.Camera ? "camera" : "")}{(g.Screen ? " screen" : "")})" : "Stream watch requested");
            AccountStatus.Text = goLive is null ? "Watching needs the call view, which this preview doesn't load." : "Going live needs the call view, which this preview doesn't load.";
            return;
        }
#endif
        if (scheduledCall is not null || pendingCall is not null || streamAttempt is not null) { AccountStatus.Text = "Finish the current call first."; return; }
        if (callMode != CallMode.Hidden && !(streamId is not null && watchedStream is { } w && w.StreamId == streamId && WatchCurrent(w)))
        { AccountStatus.Text = "Finish the current call first."; return; }
        StreamAttempt? attempt = null;
        try
        {
            if (callSession is not { } initial || !CallCurrent(initial))
                await StartCallsAsync().WaitAsync(TimeSpan.FromSeconds(15), lifetime.Token);
            if (callSession is not { } actor || !CallCurrent(actor) || actor.Mailbox.Address != expected || streamAttempt is not null)
            { AccountStatus.Text = "Calling isn't ready yet. Try again in a moment."; return; }
            attempt = new(actor, groupId); streamAttempt = attempt; UpdateCallButtons();
            AccountStatus.Text = goLive is null ? "Opening the stream…" : "Starting your stream…";
            void Check()
            {
                lifetime.Token.ThrowIfCancellationRequested();
                if (!ReferenceEquals(streamAttempt, attempt) || !CallCurrent(actor) || actor.Mailbox.Address != expected) throw new AuthorizationRetiredException();
                if (attempt.Page is { } original && !CallPageCurrent(original)) throw new AuthorizationRetiredException();
            }
            var watch = Stopwatch.StartNew();
            while (!callReady || !callConfigured || CaptureCallPage() is null)
            {
                Check(); if (watch.Elapsed > TimeSpan.FromSeconds(15)) throw new TimeoutException();
                await Task.Delay(75, lifetime.Token);
            }
            Check();
            var page = CaptureCallPage();
            if (page is null || !ReferenceEquals(page.Actor, actor)) throw new AuthorizationRetiredException();
            attempt.Page = page;
            var script = goLive is { } o ? CallBridge.GoLiveScript(groupId, groupName, o.Title, o.Camera, o.Screen, attempt.Id)
                : CallBridge.WatchStreamScript(groupId, groupName, streamId!, attempt.Id);
            if (!await RunCallScriptAsync(page, script)) throw new AuthorizationRetiredException();
            Check(); watch.Restart();
            // Going live waits for the camera prompt and the postbox; a person may take a while.
            while (!attempt.Result.Task.IsCompleted)
            {
                Check(); if (watch.Elapsed > TimeSpan.FromSeconds(90)) throw new TimeoutException();
                await Task.WhenAny(attempt.Result.Task, Task.Delay(100, lifetime.Token));
            }
            var result = await attempt.Result.Task; Check();
            DesktopLog.Write("Stream result: " + (result.State == "ok" ? "ok" : result.ErrorCode));
            if (result.State == "ok")
            {
                var id = result.Stream ?? streamId;
                if (id is not null && callMode != CallMode.Hidden) watchedStream = new(actor, page, groupId, id, null);
                AccountStatus.Text = "";
            }
            else AccountStatus.Text = result.ErrorCode is "cancelled" ? "" : GroupStreams.Explain(result.ErrorCode);
        }
        catch (OperationCanceledException) { }
        catch (Exception)
        {
            if (attempt is null || ReferenceEquals(streamAttempt, attempt) && CallCurrent(attempt.Actor))
                AccountStatus.Text = "The stream didn't confirm. Check the call view before trying again.";
        }
        finally
        {
            if (attempt is not null && ReferenceEquals(streamAttempt, attempt)) { streamAttempt = null; UpdateCallButtons(); }
            if (groupsModel.Selected?.Id == groupId) await UpdateGroupStreamAsync(force: true);
        }
    }

    private void StreamResult(CallPageTicket page, CallPageMessage result)
    {
        if (streamAttempt is { Page: { } original } attempt && result.Operation == attempt.Id && result.Type == "stream_result"
            && ReferenceEquals(original.View, page.View) && ReferenceEquals(original.Core, page.Core) && original.Document == page.Document
            && ReferenceEquals(original.Actor, page.Actor) && CallPageCurrent(original)) attempt.Result.TrySetResult(result);
    }

    private bool WatchCurrent(WatchedStream w) => ReferenceEquals(callSession, w.Actor) && CallPageCurrent(w.Page);

    // Called when the call view hides: the stream screen closed (left, stopped or ended).
    private void StreamViewClosed()
    {
        if (watchedStream is null) return;
        watchedStream = null;
        UpdateLiveControls();
    }

    private void RetireStreams()
    {
        var attempt = streamAttempt; streamAttempt = null; attempt?.Result.TrySetCanceled();
        watchedStream = null;
    }

    // The page on Windows has no event stream of group events, so a stage invitation (or being
    // taken off the stage) for the stream on screen is read from the postbox and handed to live.js.
    private async Task ForwardStageAsync()
    {
        if (watchedStream is not { } w || !WatchCurrent(w)) { watchedStream = null; return; }
        var source = streamStatusSource ?? (id => groupsModel.StreamStatusAsync(id));
        var status = await source(w.StreamId);
        if (!ReferenceEquals(watchedStream, w) || !WatchCurrent(w)) return;
        if (status is null) return; // ended or unknown: the page learns that from the calls service
        var me = w.Actor.Mailbox.Address;
        var stage = GroupStreams.StageOf(status, me);
        var forward = stage == "invited" ? "invited" : w.Stage is "invited" or "speaker" && stage is null ? "viewer" : null;
        watchedStream = w with { Stage = stage };
        if (forward is null || forward == w.Stage) return;
        var by = status.Stage?.FirstOrDefault(e => e.Address == me)?.By ?? status.By;
        DesktopLog.Write("Stream stage change forwarded: " + forward);
        await RunCallScriptAsync(w.Page, CallBridge.StageScript(w.StreamId, me, forward, by));
    }
}
