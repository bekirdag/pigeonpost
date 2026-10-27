using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Pigeonpost.Core;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow
{
    private const string MessageBindingKey = "Pigeonpost.MessageViewBinding";
    private readonly HashSet<FrameworkElement> messageOptions = [];
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
        if (messageOptions.Contains(element)) UpdateMessageOptions(element);
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

    private void MessageOptions_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement element) return;
        messageOptions.Add(element); MessageAction_Loaded(sender, e);
    }
    private void MessageOptions_Unloaded(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement element) messageOptions.Remove(element);
        MessageAction_Unloaded(sender, e);
    }
    private void UpdateMessageOptions(FrameworkElement element)
    {
        var binding = MessageBinding(element);
        var available = binding?.View is { IsCurrent: true } && binding.Row is { } row
            && (ViewModel.CanReplyToMessage || ViewModel.CanInspectMessages
                || ViewModel.CanCompose && row.CanEdit(features, DateTimeOffset.UtcNow.ToUnixTimeSeconds()));
        element.Visibility = ToVisibility(available);
    }
    private void RefreshMessageOptions()
    { foreach (var element in messageOptions.ToArray()) UpdateMessageOptions(element); }

    private void MessageOptions_Click(object sender, RoutedEventArgs e)
    {
        var model = ViewModel;
        if (dialogOpen || sender is not FrameworkElement element || MessageBinding(element) is not { View: { IsCurrent: true } view, Row: { } row }) return;
        var menu = new MenuFlyout();
        void Add(string text, RoutedEventHandler click)
        { var item = new MenuFlyoutItem { Text = text }; item.Click += click; menu.Items.Add(item); }
        void Begin(bool edit)
        {
            if (dialogOpen || !ReferenceEquals(model, ViewModel) || view.BeginAction() is not { } target) return;
            if (edit ? model.BeginEdit(target) : model.BeginReply(target)) KeepComposerFocus();
        }
        if (model.CanReplyToMessage) Add("Reply", (_, _) => Begin(edit: false));
        if (model.CanCompose && row.CanEdit(features, DateTimeOffset.UtcNow.ToUnixTimeSeconds())) Add("Edit message", (_, _) => Begin(edit: true));
        if (model.CanInspectMessages) Add("Message info", async (_, _) =>
        {
            if (!ReferenceEquals(model, ViewModel) || view.BeginAction() is not { } target) return;
            await ShowMessageInfoAsync(model, target);
        });
        if (menu.Items.Count > 0) menu.ShowAt(element);
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
                if (receipt.DeliveredAt is null && receipt.SeenAt is null) Line(target.Message.ReceiptLabel);
            }
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
