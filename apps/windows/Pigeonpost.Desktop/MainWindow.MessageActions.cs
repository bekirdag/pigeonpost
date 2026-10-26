using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Pigeonpost.Core;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow
{
    private const string MessageBindingKey = "Pigeonpost.MessageViewBinding";
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
}
