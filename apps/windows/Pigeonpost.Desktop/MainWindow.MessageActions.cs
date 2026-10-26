using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Pigeonpost.Core;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow
{
    private void MessageAction_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement element) return;
        // WinUI owns the control projection. Keep the CLR target on its Tag, as the message
        // binding already does, rather than keying event-sender wrappers by managed identity.
        var row = element.Tag switch
        {
            ThreadMessage message => message,
            MessageTarget prior when prior.IsAccountCurrent => prior.Message,
            _ => null
        };
        if (element.Tag is MessageTarget old) old.Retire();
        element.Tag = row is null ? null : ViewModel.CaptureMessageTarget(row);
    }
    private void MessageAction_Unloaded(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: MessageTarget target }) target.Retire();
    }
    private void ReplyParent_Click(object sender, RoutedEventArgs e)
    {
        var model = ViewModel;
        if (sender is FrameworkElement { Tag: MessageTarget target }
            && model.OpenReplyParent(target) is { } parent && ReferenceEquals(model, ViewModel) && target.IsAccountCurrent)
        {
            QueueScroll(parent, leading: true, select: true);
            MessageList.Focus(FocusState.Programmatic);
        }
    }
}
