using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Animation;
using Pigeonpost.Core;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow
{
    private const string MessageBindingKey = "Pigeonpost.MessageViewBinding";
    // §14C.7: controls whose availability follows the account, the features and the edit window.
    private enum MessageControl { Edit, Reply, Receipt }
    private readonly Dictionary<FrameworkElement, MessageControl> messageControls = [];
    private sealed class MessageControlBinding
    {
        internal MessageViewBinding? View;
        internal ThreadMessage? Row;
        internal long? TagSubscription;
    }
    private static MessageControlBinding? MessageBinding(FrameworkElement element) =>
        element.Resources.ContainsKey(MessageBindingKey) ? element.Resources[MessageBindingKey] as MessageControlBinding : null;

    private void MessageAction_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement element) return;
        var binding = MessageBinding(element);
        if (binding is null)
        {
            binding = new MessageControlBinding();
            element.Resources[MessageBindingKey] = binding;
        }
        if (binding.TagSubscription is null)
            binding.TagSubscription = element.RegisterPropertyChangedCallback(FrameworkElement.TagProperty,
                (control, _) => BindMessageControl((FrameworkElement)control, newRow: true));
        BindMessageControl(element, newRow: false);
    }
    private void BindMessageControl(FrameworkElement element, bool newRow)
    {
        if (MessageBinding(element) is not { } binding) return;
        // Tag remains owned by x:Bind. A separate native resource retains the CLR view identity
        // through polling/recycling without relying on managed event-wrapper identity.
        if (!newRow && binding.View is { IsAccountCurrent: false } && ReferenceEquals(binding.Row, element.Tag))
        { binding.View.Retire(); return; }
        binding.View?.Retire();
        binding.Row = element.Tag as ThreadMessage;
        binding.View = binding.Row is { } row ? ViewModel.CaptureMessageView(row) : null;
        if (messageControls.TryGetValue(element, out var kind)) UpdateMessageControl(element, kind);
    }
    private void MessageAction_Unloaded(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement element || MessageBinding(element) is not { } binding) return;
        binding.View?.Retire();
        if (binding.TagSubscription is { } token) element.UnregisterPropertyChangedCallback(FrameworkElement.TagProperty, token);
        binding.TagSubscription = null;
    }
    private void ReplyParent_Click(object sender, RoutedEventArgs e)
    {
        var model = ViewModel;
        if (sender is not FrameworkElement element || MessageBinding(element)?.View?.BeginAction() is not { } target)
        { DesktopLog.Write("Quoted message action refused: view changed"); return; }
        if (model.OpenReplyParent(target) is { } parent && ReferenceEquals(model, ViewModel) && target.IsAccountCurrent)
        {
            DesktopLog.Write("Quoted message parent selected");
            QueueScroll(parent, leading: true, select: true);
            MessageList.Focus(FocusState.Programmatic);
        }
        else DesktopLog.Write("Quoted message action refused: parent unavailable");
    }

    private void RegisterMessageControl(object sender, RoutedEventArgs e, MessageControl kind)
    {
        if (sender is not FrameworkElement element) return;
        messageControls[element] = kind; MessageAction_Loaded(sender, e);
    }
    private void MessageEdit_Loaded(object sender, RoutedEventArgs e) => RegisterMessageControl(sender, e, MessageControl.Edit);
    private void MessageReply_Loaded(object sender, RoutedEventArgs e) => RegisterMessageControl(sender, e, MessageControl.Reply);
    private void ReceiptMark_Loaded(object sender, RoutedEventArgs e) => RegisterMessageControl(sender, e, MessageControl.Receipt);
    private void MessageControl_Unloaded(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement element) messageControls.Remove(element);
        MessageAction_Unloaded(sender, e);
    }
    private bool CanEditRow(InboxViewModel model, ThreadMessage row) =>
        model.CanCompose && row.CanEdit(features, DateTimeOffset.UtcNow.ToUnixTimeSeconds());
    private void UpdateMessageControl(FrameworkElement element, MessageControl kind)
    {
        var binding = MessageBinding(element);
        var current = binding?.View is { IsCurrent: true } && binding.Row is { IsSystem: false };
        var row = binding?.Row;
        switch (kind)
        {
            case MessageControl.Edit:
                element.Visibility = ToVisibility(current && CanEditRow(ViewModel, row!)); break;
            case MessageControl.Reply:
                element.Visibility = ToVisibility(current && ViewModel.CanReplyToMessage); break;
            case MessageControl.Receipt:
                // The marks always show; they only open message info when the postbox can provide it.
                var inspectable = current && ViewModel.CanInspectMessages;
                element.IsHitTestVisible = inspectable;
                if (element is Control control) control.IsTabStop = inspectable;
                break;
        }
    }
    private void RefreshMessageOptions()
    { foreach (var (element, kind) in messageControls.ToArray()) UpdateMessageControl(element, kind); }

    private void BeginMessageAction(FrameworkElement element, bool edit)
    {
        var model = ViewModel;
        if (dialogOpen || MessageBinding(element) is not { View: { IsCurrent: true } view } || view.BeginAction() is not { } target) return;
        if (!ReferenceEquals(model, ViewModel)) return;
        if (edit ? model.BeginEdit(target) : model.BeginReply(target)) KeepComposerFocus();
    }
    private void MessageEdit_Click(object sender, RoutedEventArgs e)
    { if (sender is FrameworkElement element) BeginMessageAction(element, edit: true); }
    private void MessageReply_Click(object sender, RoutedEventArgs e)
    { if (sender is FrameworkElement element) BeginMessageAction(element, edit: false); }
    private async void ReceiptMark_Click(object sender, RoutedEventArgs e)
    {
        var model = ViewModel;
        if (dialogOpen || !model.CanInspectMessages || sender is not FrameworkElement element
            || MessageBinding(element) is not { View: { IsCurrent: true } view } || view.BeginAction() is not { } target) return;
        await ShowMessageInfoAsync(model, target);
    }

    // The right-click menu keeps the full list (§14C.7): the row's actions come first, then the
    // template's own items (copy original, save attachments, block sender).
    private static readonly object DynamicMenuItem = new();
    private void MessageMenu_Opening(object? sender, object e)
    {
        if (sender is not MenuFlyout menu) return;
        foreach (var stale in menu.Items.Where(i => ReferenceEquals(i.Tag, DynamicMenuItem)).ToArray())
            menu.Items.Remove(stale);
        var model = ViewModel;
        if (dialogOpen || menu.Target is not FrameworkElement element || MessageBinding(element) is not { View: { IsCurrent: true } view, Row: { IsSystem: false } row }) return;
        var index = 0;
        void Add(string text, Action click)
        {
            var item = new MenuFlyoutItem { Text = text, Tag = DynamicMenuItem };
            item.Click += (_, _) => click();
            menu.Items.Insert(index++, item);
        }
        void Begin(bool edit)
        {
            if (dialogOpen || !ReferenceEquals(model, ViewModel) || view.BeginAction() is not { } target) return;
            if (edit ? model.BeginEdit(target) : model.BeginReply(target)) KeepComposerFocus();
        }
        if (model.CanReplyToMessage) Add("Reply", () => Begin(edit: false));
        Add("Copy message", () => CopyText(row.CopyText));
        if (CanEditRow(model, row)) Add("Edit message", () => Begin(edit: true));
        if (model.CanInspectMessages) Add("Message info", async () =>
        {
            if (!ReferenceEquals(model, ViewModel) || view.BeginAction() is not { } target) return;
            await ShowMessageInfoAsync(model, target);
        });
        menu.Items.Insert(index, new MenuFlyoutSeparator { Tag = DynamicMenuItem });
    }

    // Desktop: the row stays quiet (70%) until the message is hovered or holds focus.
    private readonly HashSet<Panel> hoveredMessages = [];
    private static FrameworkElement? MessageActionRow(Panel stack) => stack.Children.Count > 1 ? stack.Children[^1] as FrameworkElement : null;
    private void UpdateMessageRowEmphasis(Panel stack)
    {
        if (MessageActionRow(stack) is not { } row) return;
        var focused = stack.XamlRoot is { } root && FocusManager.GetFocusedElement(root) is DependencyObject focus && IsWithin(focus, stack);
        row.Opacity = hoveredMessages.Contains(stack) || focused ? 1 : 0.7;
    }
    private static bool IsWithin(DependencyObject? node, DependencyObject ancestor)
    {
        for (; node is not null; node = VisualTreeHelper.GetParent(node)) if (ReferenceEquals(node, ancestor)) return true;
        return false;
    }
    private void MessageStack_PointerEntered(object sender, PointerRoutedEventArgs e)
    { if (sender is Panel stack) { hoveredMessages.Add(stack); UpdateMessageRowEmphasis(stack); } }
    private void MessageStack_PointerExited(object sender, PointerRoutedEventArgs e)
    { if (sender is Panel stack) { hoveredMessages.Remove(stack); UpdateMessageRowEmphasis(stack); } }
    private void MessageStack_FocusChanged(object sender, RoutedEventArgs e)
    { if (sender is Panel stack) DispatcherQueue.TryEnqueue(() => UpdateMessageRowEmphasis(stack)); }

    // §14C.6: the green gear turns slowly while an agent works. When Windows animation effects are
    // off, it stands still and the word "Working…" says it instead.
    private readonly Dictionary<FrameworkElement, Storyboard?> agentGears = [];
    private Windows.UI.ViewManagement.UISettings? motionSettings;
    private bool AnimationsEnabled
    {
        get
        {
            try { return (motionSettings ??= CreateMotionSettings()).AnimationsEnabled; }
            catch (Exception ex) { DesktopLog.Error("Animation preference unavailable", ex); return true; }
        }
    }
    private Windows.UI.ViewManagement.UISettings CreateMotionSettings()
    {
        var settings = new Windows.UI.ViewManagement.UISettings();
        try { settings.AnimationsEnabledChanged += (_, _) => DispatcherQueue.TryEnqueue(RefreshAgentGears); }
        catch (Exception ex) { DesktopLog.Error("Animation preference changes unavailable", ex); }
        return settings;
    }
    private void AgentGear_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement element) return;
        agentGears.TryAdd(element, null); ApplyAgentGear(element);
    }
    private void AgentGear_Unloaded(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement element) return;
        if (agentGears.Remove(element, out var spin)) spin?.Stop();
    }
    private void RefreshAgentGears()
    { foreach (var element in agentGears.Keys.ToArray()) ApplyAgentGear(element); }
    private void ApplyAgentGear(FrameworkElement element)
    {
        var animate = AnimationsEnabled;
        if (element is TextBlock word) { word.Visibility = ToVisibility(!animate); return; }
        agentGears.TryGetValue(element, out var spin);
        if (animate && spin is null && element.RenderTransform is RotateTransform rotate)
        {
            var turn = new DoubleAnimation { From = 0, To = 360, Duration = new Duration(TimeSpan.FromSeconds(3)), RepeatBehavior = RepeatBehavior.Forever };
            Storyboard.SetTarget(turn, rotate); Storyboard.SetTargetProperty(turn, "Angle");
            spin = new Storyboard(); spin.Children.Add(turn); spin.Begin();
            agentGears[element] = spin;
        }
        else if (!animate && spin is not null)
        {
            spin.Stop(); agentGears[element] = null;
            if (element.RenderTransform is RotateTransform still) still.Angle = 0;
        }
    }

    private void CancelComposerAction_Click(object sender, RoutedEventArgs e)
    { if (!dialogOpen && ViewModel.CancelComposerAction()) KeepComposerFocus(); }

    private static string MessageInfoTime(long? at)
    {
        if (!at.HasValue || at.Value < -62135596800L || at.Value > 253402300799L) return "Unknown";
        return DateTimeOffset.FromUnixTimeSeconds(at.Value).ToLocalTime().ToString("g");
    }
    private async Task ShowMessageInfoAsync(InboxViewModel model, MessageTarget target)
    {
        if (dialogOpen) return;
        dialogOpen = true;
        try
        {
            var info = await model.ReadMessageInfoAsync(target, lifetime.Token);
            if (info is null || !ReferenceEquals(model, ViewModel) || !target.IsCurrent) return;
            var panel = new StackPanel { Spacing = 12, MaxWidth = 480 };
            void Line(string text) => panel.Children.Add(new TextBlock { Text = text, TextWrapping = TextWrapping.Wrap });
            void Content(string text, string name)
            {
                var box = new TextBox { Text = text, IsReadOnly = true, AcceptsReturn = true, TextWrapping = TextWrapping.Wrap, MaxHeight = 160 };
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(box, name); panel.Children.Add(box);
            }
            Line("Sent " + MessageInfoTime(info.SentAt ?? target.Message.At));
            if (target.Message.IsOutgoing && info.Receipt is { } receipt)
            {
                if (receipt.DeliveredAt is { } delivered) Line("Delivered " + MessageInfoTime(delivered));
                if (receipt.SeenAt is { } seen) Line("Seen " + MessageInfoTime(seen));
                if (receipt.DeliveredAt is null && receipt.SeenAt is null && target.Message.Receipt?.State == "sent") Line("Not delivered yet");
            }
            if (target.Message.IsOutgoing && (info.AgentStatus ?? target.Message.AgentStatus) is { State: { } agent } status)
                Line((agent switch { "working" => "Agent working on this", "done" => "Agent finished", "failed" => "Agent run failed", _ => "Agent status: " + agent })
                    + (status.At is { } at ? " · " + MessageInfoTime(at) : ""));
            Content(target.Message.DisplayBody, "Message content");
            var versions = info.Versions ?? [];
            if (versions.Count > 0)
            {
                Line(versions.Count == 1 ? "Version history · 1 version" : $"Version history · {versions.Count} versions");
                if (versions.Count > 20) Line("Showing the latest 20 versions.");
                foreach (var version in versions.OrderByDescending(v => v.At).Take(20))
                { Line(MessageInfoTime(version.At)); Content(RequestEnvelope.DisplayText(version.Body), "Message version"); }
            }
            await new ContentDialog { XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = "Message info",
                Content = new ScrollViewer { Content = panel, MaxHeight = 480, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled }, CloseButtonText = "Done" }.ShowAsync();
        }
        catch (OperationCanceledException) { }
        catch (Exception ex) { DesktopLog.Error("Message information unavailable", ex); if (ReferenceEquals(model, ViewModel)) AccountStatus.Text = "Could not load message info. Try again."; }
        finally { dialogOpen = false; KeepComposerFocus(); }
    }
}
