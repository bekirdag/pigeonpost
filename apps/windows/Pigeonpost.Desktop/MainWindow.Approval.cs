using System.Diagnostics;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Pigeonpost.Core;
using Pigeonpost.Core.Enterprise;
using Windows.System;

namespace Pigeonpost.Desktop;

// Approve a machine (docs/device-pairing-native.md). The code and confirmation stay in this
// window's memory. A decision needs a deliberate pointer hold or a confirmed keyboard/Narrator
// action on the currently reviewed screen. Verification never approves by itself.
public sealed partial class MainWindow
{
    private DeviceApprovalStore approvalStore = null!;
    private DispatcherTimer? approvalTimer;
    private ApprovalView? approvalView;
#if UI_TESTS
    private NativeApprovalAccount? approvalAccount;
    private NativeApprovalFixture? approvalFixture;
#endif
    private const double HoldMilliseconds = 1500;

    private sealed class ApprovalView
    {
        public required TextBox Code;
        public required Button Review, ChangeCode, Approve, Deny, Verify, CancelVerify, Check;
        public required TextBlock Status, Note, Organization, Agent, AgentKind, Machine, Access, Network, Expiry, Confirmation, Hint;
        public required StackPanel Details, Decision;
        public required Border Hold;
        public required ProgressBar HoldProgress;
        public required ProgressRing Busy;
        public DispatcherTimer? HoldTimer;
        public Stopwatch HoldClock = new();
        public (string Proof, string Context)? Holding;
    }

    private void CreateApprovalStore()
    {
#if UI_TESTS
        if (approvalFixture is { } fixture && approvalAccount is { } account)
        {
            approvalStore = new(fixture, () => new(account.Owner, account.Lease));
            approvalStore.PropertyChanged += (_, _) => RenderApproval();
            return;
        }
#endif
        approvalStore = new(postbox, () => session.AcceptedAuthorization is { IsActive: true } grant
            && session.RecoveryOwnerKey is { } owner ? new(owner, grant) : null);
        approvalStore.PropertyChanged += (_, _) => RenderApproval();
    }

    private void CloseApproval()
    {
        StopHold(showHint: false);
        // Leaving the page forgets the code, unless a browser verification is still in progress.
        if (approvalStore.State.Phase != DeviceApprovalPhase.Verifying) approvalStore.Close();
    }

    private FrameworkElement BuildApprovalPage()
    {
        var frame = PageFrame("Approve a machine", out _, out var body);
        approvalStore.Synchronize();
        TextBlock Text(string text = "", double size = 14, bool wrap = true, bool strong = false)
            => new() { Text = text, FontSize = size, TextWrapping = wrap ? TextWrapping.Wrap : TextWrapping.NoWrap, IsTextSelectionEnabled = false,
                FontWeight = strong ? Microsoft.UI.Text.FontWeights.SemiBold : Microsoft.UI.Text.FontWeights.Normal };
        Button Action(string label, string? automation = null, bool accent = false)
        {
            var button = new Button { Content = label, MinHeight = 34, Padding = new Thickness(14, 6, 14, 6) };
            if (accent) button.Style = (Style)Application.Current.Resources["AccentButtonStyle"];
            AutomationProperties.SetName(button, automation ?? label);
            return button;
        }
        var page = new StackPanel { Spacing = 14, Padding = new Thickness(24, 4, 24, 24), MaxWidth = 640, HorizontalAlignment = HorizontalAlignment.Left };
        page.Children.Add(Text("Approve a machine that is signing in as one of your organization's agents. Enter the code shown in its terminal."));
        var entry = new Grid { ColumnSpacing = 8 };
        entry.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        entry.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var code = new TextBox { PlaceholderText = "ABCD-EFGH, or the pair.html link", MaxLength = 4096, IsSpellCheckEnabled = false, IsTextPredictionEnabled = false };
        AutomationProperties.SetName(code, "Machine code");
        var review = Action("Review request", accent: true);
        Grid.SetColumn(review, 1);
        entry.Children.Add(code); entry.Children.Add(review);
        page.Children.Add(entry);
        var busy = new ProgressRing { IsActive = false, Width = 20, Height = 20, HorizontalAlignment = HorizontalAlignment.Left };
        AutomationProperties.SetName(busy, "Working");
        page.Children.Add(busy);
        var status = Text(); AutomationProperties.SetLiveSetting(status, AutomationLiveSetting.Polite);
        var note = Text(); AutomationProperties.SetLiveSetting(note, AutomationLiveSetting.Assertive);
        page.Children.Add(status); page.Children.Add(note);

        var details = new StackPanel { Spacing = 8, Visibility = Visibility.Collapsed };
        var organization = Text(); var agent = Text(size: 22, strong: true);
        AutomationProperties.SetHeadingLevel(agent, AutomationHeadingLevel.Level2);
        var agentKind = Text(); var machine = Text(); var access = Text(); var network = Text(); var expiry = Text();
        details.Children.Add(organization); details.Children.Add(agent); details.Children.Add(agentKind);
        details.Children.Add(machine); details.Children.Add(access); details.Children.Add(network); details.Children.Add(expiry);
        var confirmationBox = new StackPanel { Spacing = 4, Padding = new Thickness(16, 12, 16, 12), CornerRadius = new CornerRadius(8),
            Background = (Brush)Application.Current.Resources["CardBackgroundFillColorDefaultBrush"], BorderThickness = new Thickness(1),
            BorderBrush = (Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"] };
        confirmationBox.Children.Add(Text("Confirmation number", strong: true));
        var confirmation = new TextBlock { FontFamily = new FontFamily("Consolas"), FontSize = 36, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, IsTextSelectionEnabled = false };
        confirmationBox.Children.Add(confirmation);
        confirmationBox.Children.Add(Text("Check that your terminal shows the same number. If it doesn't, or you didn't start this, choose Deny."));
        details.Children.Add(confirmationBox);

        var decision = new StackPanel { Spacing = 10 };
        var holdProgress = new ProgressBar { Minimum = 0, Maximum = 1, Value = 0, Height = 4, Margin = new Thickness(0, 6, 0, 0) };
        var holdLabel = new StackPanel { Padding = new Thickness(18, 10, 18, 10) };
        holdLabel.Children.Add(new TextBlock { Text = "Hold to approve", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, HorizontalAlignment = HorizontalAlignment.Center });
        holdLabel.Children.Add(holdProgress);
        var hold = new Border { Child = holdLabel, CornerRadius = new CornerRadius(8), MinWidth = 220, HorizontalAlignment = HorizontalAlignment.Left,
            Background = (Brush)Application.Current.Resources["AccentFillColorDefaultBrush"] };
        ((TextBlock)holdLabel.Children[0]).Foreground = (Brush)Application.Current.Resources["TextOnAccentFillColorPrimaryBrush"];
        AutomationProperties.SetName(hold, "Hold to approve");
        AutomationProperties.SetHelpText(hold, "Press and hold for about 1.5 seconds with a mouse, pen or touch. Keyboard and Narrator users can choose Approve with confirmation.");
        var hint = Text(); hint.Visibility = Visibility.Collapsed; AutomationProperties.SetLiveSetting(hint, AutomationLiveSetting.Polite);
        var approve = Action("Approve with confirmation…");
        var deny = Action("Deny");
        var buttons = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        buttons.Children.Add(approve); buttons.Children.Add(deny);
        decision.Children.Add(hold); decision.Children.Add(hint); decision.Children.Add(buttons);
        details.Children.Add(decision);
        var verify = Action("Verify sign-in in your browser", accent: true);
        var cancelVerify = Action("Cancel verification");
        var check = Action("Check status");
        var changeCode = Action("Enter a different code");
        var more = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        more.Children.Add(verify); more.Children.Add(cancelVerify); more.Children.Add(check); more.Children.Add(changeCode);
        page.Children.Add(details); page.Children.Add(more);
        body.Children.Add(new ScrollViewer { Content = page, VerticalScrollBarVisibility = ScrollBarVisibility.Auto });

        var view = approvalView = new ApprovalView
        {
            Code = code, Review = review, ChangeCode = changeCode, Approve = approve, Deny = deny, Verify = verify, CancelVerify = cancelVerify, Check = check,
            Status = status, Note = note, Organization = organization, Agent = agent, AgentKind = agentKind, Machine = machine, Access = access,
            Network = network, Expiry = expiry, Confirmation = confirmation, Hint = hint, Details = details, Decision = decision,
            Hold = hold, HoldProgress = holdProgress, Busy = busy
        };
        void Submit() { view.Note.Text = ""; approvalStore.Resolve(code.Text, approvalStore.State.ContextId); RenderApproval(); }
        review.Click += (_, _) => Submit();
        code.KeyDown += (_, e) => { if (e.Key == VirtualKey.Enter) { e.Handled = true; Submit(); } };
        changeCode.Click += (_, _) => { approvalStore.ChangeCode(approvalStore.State.ContextId); code.Text = ""; code.Focus(FocusState.Programmatic); };
        check.Click += (_, _) => approvalStore.CheckStatus(approvalStore.State.ContextId);
        deny.Click += (_, _) => approvalStore.Deny(approvalStore.State.ProofId, approvalStore.State.ContextId);
        approve.Click += async (_, _) => await ConfirmApprovalAsync();
        verify.Click += (_, _) => StartApprovalVerification();
        cancelVerify.Click += (_, _) => approvalStore.CancelVerification(approvalStore.State.ContextId);
        hold.PointerPressed += (_, e) =>
        {
            var state = approvalStore.State;
            if (!approvalStore.CanApprove(state.ProofId, state.ContextId)) return;
            e.Handled = true; hold.CapturePointer(e.Pointer);
            view.Holding = (state.ProofId, state.ContextId); view.HoldClock.Restart(); view.Hint.Visibility = Visibility.Collapsed;
            view.HoldTimer?.Stop();
            view.HoldTimer = new DispatcherTimer { Interval = TimeSpan.FromMilliseconds(40) };
            view.HoldTimer.Tick += (_, _) =>
            {
                if (view.Holding is not { } held) { view.HoldTimer?.Stop(); return; }
                var progress = view.HoldClock.Elapsed.TotalMilliseconds / HoldMilliseconds;
                view.HoldProgress.Value = Math.Min(1, progress);
                if (progress < 1) return;
                view.HoldTimer?.Stop(); view.Holding = null; hold.ReleasePointerCaptures(); view.HoldProgress.Value = 0;
                approvalStore.Approve(held.Proof, held.Context); // Refused if the reviewed screen changed meanwhile.
            };
            view.HoldTimer.Start();
        };
        void Released(object _, PointerRoutedEventArgs e) { if (view.Holding is not null) { e.Handled = true; StopHold(showHint: true); } }
        hold.PointerReleased += Released; hold.PointerCanceled += Released; hold.PointerCaptureLost += Released;
        approvalTimer ??= CreateApprovalTimer();
        approvalTimer.Start();
        RenderApproval();
        return frame;
    }

    private DispatcherTimer CreateApprovalTimer()
    {
        var timer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(1) };
        timer.Tick += (_, _) =>
        {
            if (approvalView is null || sideView != SideView.Machine) { timer.Stop(); return; }
            approvalStore.Synchronize();
            approvalStore.RefreshExpiry();
            RenderApproval();
        };
        return timer;
    }

    private void StopHold(bool showHint)
    {
        if (approvalView is not { } view) return;
        var wasHolding = view.Holding is not null;
        view.HoldTimer?.Stop(); view.Holding = null; view.HoldProgress.Value = 0;
        try { view.Hold.ReleasePointerCaptures(); } catch (Exception) { }
        if (showHint && wasHolding)
        {
            view.Hint.Text = "Keep holding for about 1.5 seconds to approve, or choose Approve with confirmation.";
            view.Hint.Visibility = Visibility.Visible;
        }
    }

    private static string DescribeAccess(IReadOnlyList<string> capabilities) => string.Join("; ", capabilities.Select(c => c == "mailbox"
        ? "Read and send this agent's messages" : "Unsupported access: " + c));

    private void RenderApproval()
    {
        if (approvalView is not { } view) return;
        var state = approvalStore.State;
        var current = approvalStore.IsCurrent(state.ContextId);
        view.Busy.IsActive = state.Busy;
        view.Busy.Visibility = state.Busy ? Visibility.Visible : Visibility.Collapsed;
        view.Status.Text = state.Phase switch
        {
            DeviceApprovalPhase.Resolving => "Checking the request…",
            DeviceApprovalPhase.Approving => "Approving…",
            DeviceApprovalPhase.Denying => "Denying…",
            DeviceApprovalPhase.Verifying => "Finish verifying your sign-in in your browser. You will review this request again afterwards.",
            _ => state.Message ?? (current ? "" : "Sign in to review a machine.")
        };
        if (state.RetryAt is { } until && until > DateTimeOffset.UtcNow.ToUnixTimeSeconds() && state.Phase != DeviceApprovalPhase.Resolving)
            view.Status.Text = $"{state.Message ?? "Too many attempts."} Try again in {until - DateTimeOffset.UtcNow.ToUnixTimeSeconds()} s.";
        AutomationProperties.SetName(view.Status, view.Status.Text);
        view.Status.Visibility = view.Status.Text.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
        view.Note.Visibility = view.Note.Text.Length > 0 ? Visibility.Visible : Visibility.Collapsed;
        var reviewing = state.Preview is not null && state.Phase is DeviceApprovalPhase.Review or DeviceApprovalPhase.Approving or DeviceApprovalPhase.Denying
            or DeviceApprovalPhase.Verifying or DeviceApprovalPhase.Unconfirmed;
        view.Code.IsEnabled = view.Review.IsEnabled = current && !state.Busy && !state.HasCode;
        view.Details.Visibility = reviewing ? Visibility.Visible : Visibility.Collapsed;
        if (state.Preview is { } preview)
        {
            view.Organization.Text = "Organization: " + preview.Org.Name;
            view.Agent.Text = preview.Handle;
            view.AgentKind.Text = preview.CreatesAgent == true
                ? "Approving creates this new agent in your organization."
                : "Existing agent · " + preview.Address;
            view.Machine.Text = "Machine: " + (preview.Label ?? "not named");
            view.Access.Text = "Requested access: " + DescribeAccess(preview.Capabilities);
            view.Network.Text = preview.IpCountry is { } country
                ? $"Network location: {country}. This comes from the network and is not proof of anything."
                : "Network location: unknown. Only the confirmation number ties this request to the terminal.";
            var left = preview.ExpiresAt - DateTimeOffset.UtcNow.ToUnixTimeSeconds();
            view.Expiry.Text = left > 0 ? $"Expires in {left / 60}:{left % 60:00}" : "Expired";
            view.Confirmation.Text = preview.DisplayConfirmation;
            AutomationProperties.SetName(view.Confirmation, "Confirmation number " + preview.DisplayConfirmation);
        }
        var canApprove = approvalStore.CanApprove(state.ProofId, state.ContextId);
        view.Decision.Visibility = state.Phase == DeviceApprovalPhase.Review && !state.NeedsVerification ? Visibility.Visible : Visibility.Collapsed;
        view.Approve.IsEnabled = canApprove;
        view.Hold.Opacity = canApprove ? 1 : 0.45;
        view.Deny.IsEnabled = approvalStore.CanDeny(state.ProofId, state.ContextId);
        if (!canApprove && view.Holding is not null) StopHold(showHint: false);
        view.Verify.Visibility = state.NeedsVerification ? Visibility.Visible : Visibility.Collapsed;
        view.Verify.IsEnabled = current && state.NeedsVerification;
        view.CancelVerify.Visibility = state.Phase == DeviceApprovalPhase.Verifying ? Visibility.Visible : Visibility.Collapsed;
        view.Check.Visibility = state.HasCode && state.Phase is DeviceApprovalPhase.Unconfirmed or DeviceApprovalPhase.Idle ? Visibility.Visible : Visibility.Collapsed;
        view.Check.IsEnabled = current && !state.Busy;
        view.ChangeCode.Visibility = state.HasCode && !state.Busy ? Visibility.Visible : Visibility.Collapsed;
    }

    private async Task ConfirmApprovalAsync()
    {
        var state = approvalStore.State;
        if (dialogOpen || state.Preview is not { } preview || !approvalStore.CanApprove(state.ProofId, state.ContextId)) return;
        dialogOpen = true;
        try
        {
            var content = new StackPanel { Spacing = 8 };
            content.Children.Add(new TextBlock { Text = preview.CreatesAgent == true ? "This creates a new agent: " + preview.Handle : "Agent: " + preview.Handle, TextWrapping = TextWrapping.Wrap });
            content.Children.Add(new TextBlock { Text = "Confirmation number " + preview.DisplayConfirmation, FontFamily = new FontFamily("Consolas"), FontSize = 20 });
            content.Children.Add(new TextBlock { Text = "Approve only if your terminal shows this number.", TextWrapping = TextWrapping.Wrap });
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = "Approve this machine?", Content = content,
                PrimaryButtonText = "Approve", CloseButtonText = "Cancel", DefaultButton = ContentDialogButton.Close
            };
            if (await dialog.ShowAsync() == ContentDialogResult.Primary) approvalStore.Approve(state.ProofId, state.ContextId);
        }
        finally { dialogOpen = false; }
    }

    private string? VerificationBlocker()
    {
        if (dialogOpen) return "Close the open dialog first.";
        if (callMode != CallMode.Hidden || callRinging || pendingCall is not null || scheduledCall is not null) return "Finish your call before verifying your sign-in.";
        if (!ViewModel.ReadyForGrantReplacement) return "Finish sending, editing or replying to a message before verifying your sign-in.";
        if (!groupsModel.ReadyForGrantReplacement) return "Wait for your group message to finish sending before verifying your sign-in.";
        return null;
    }

    private void StartApprovalVerification()
    {
        if (approvalView is not { } view) return;
        var state = approvalStore.State;
        if (VerificationBlocker() is { } reason) { view.Note.Text = reason; RenderApproval(); return; }
        view.Note.Text = "";
        approvalStore.VerifySignIn(state.ProofId, state.ContextId, VerifyForApprovalAsync);
    }

    private async Task<AuthorizationLease> VerifyForApprovalAsync(StrongSignInRequirement requirement, AuthorizationLease expected, CancellationToken ct)
    {
#if UI_TESTS
        if (approvalFixture is { } fixture)
        {
            await Task.Delay(400, ct);
            var adopted = fixture.Verify(requirement, expected);
            await RebindVerifiedAccountAsync(expected);
            return adopted;
        }
#endif
        var accepted = await StrongSignInBroker.VerifyAsync(session, requirement, expected, uri => Launcher.LaunchUriAsync(uri).AsTask(), ct);
        await RebindVerifiedAccountAsync(expected);
        return accepted;
    }

    // After a verified same-person adoption: carry unsent drafts, then rebind every grant-bound
    // view (inbox, events, calls, calendar, groups) to the new grant through the normal start path.
    private async Task RebindVerifiedAccountAsync(AuthorizationLease prior)
    {
        try
        {
            var inbox = ViewModel.CarryUnsentWork(prior);
            groupsModel.CarryUnsentWork(prior);
            DesktopLog.Write(inbox ? "Rebinding after verified sign-in with unsent work kept" : "Rebinding after verified sign-in");
            refreshTimer?.Stop();
            accountEvents?.Cancel();
            await OpenInboxAsync();
            if (groupsModel.HasCarriedDraft) AccountStatus.Text = "Your unsent group message was kept. Open that group to continue it.";
        }
        catch (Exception ex) { DesktopLog.Error("Rebinding after verified sign-in failed", ex); AccountStatus.Text = "Reload your account to finish reconnecting."; }
    }
}
