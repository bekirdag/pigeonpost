using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Pigeonpost.Core;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow
{
    private InboxViewModel CreateLiveInboxViewModel()
    {
        var folder = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Pigeonpost", "deliveries");
        var coordinator = new MessageDeliveryCoordinator(postbox, new FileMessageAttemptStore(folder, new DpapiProtector()));
        return new(postbox, deliveryCoordinator: coordinator);
    }

    private async void ReviewPendingDeliveries_Click(object sender, RoutedEventArgs e)
    {
        var model = ViewModel;
        if (dialogOpen || !model.CanReviewPendingDeliveries) return;
        dialogOpen = true;
        try
        {
            await model.RefreshPendingDeliveriesAsync();
            if (!ReferenceEquals(model, ViewModel) || !model.CanReviewPendingDeliveries) return;
            var choices = model.PendingDeliveries.ToArray();
            if (choices.Length == 0)
            {
                if (model.HasError) return;
                await new ContentDialog { XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(),
                    Title = "Pending deliveries", Content = "There are no pending deliveries in this inbox.", CloseButtonText = "Close" }.ShowAsync();
                return;
            }
            PendingDeliveryView? selected;
            if (choices.Length == 1) selected = choices[0];
            else
            {
                var picker = new ComboBox { Header = "Delivery to review", ItemsSource = choices, DisplayMemberPath = nameof(PendingDeliveryView.Label),
                    SelectedIndex = 0, HorizontalAlignment = HorizontalAlignment.Stretch, MinWidth = 280 };
                Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(picker, "Delivery to review");
                var chooser = new ContentDialog { XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(),
                    Title = "Pending deliveries", Content = picker, PrimaryButtonText = "Review", CloseButtonText = "Cancel" };
                if (await chooser.ShowAsync() != ContentDialogResult.Primary) return;
                selected = picker.SelectedItem as PendingDeliveryView;
            }
            if (!ReferenceEquals(model, ViewModel) || selected?.BeginAction() is not { } target) return;
            var panel = new StackPanel { Spacing = 10, MaxWidth = 480 };
            panel.Children.Add(new TextBlock { Text = selected.Recipient, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, TextWrapping = TextWrapping.Wrap });
            panel.Children.Add(new TextBlock { Text = selected.CanonicalRecipient, FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Consolas"),
                FontSize = 12, IsTextSelectionEnabled = true, TextWrapping = TextWrapping.Wrap });
            panel.Children.Add(new TextBlock { Text = selected.Subject + " · " + selected.Details, TextWrapping = TextWrapping.Wrap });
            var body = new TextBox { Text = selected.Body, IsReadOnly = true, AcceptsReturn = true, TextWrapping = TextWrapping.Wrap, MinHeight = 72, MaxHeight = 220 };
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(body, "Saved delivery content");
            panel.Children.Add(body);
            panel.Children.Add(new TextBlock { Text = "This may already have been delivered. Retry uses the saved request and avoids an extra copy. Discard removes local recovery and cannot undo a delivery.",
                TextWrapping = TextWrapping.Wrap, FontSize = 13 });
            var dialog = new ContentDialog { XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(),
                Title = "Pending delivery", Content = panel, PrimaryButtonText = "Retry delivery", SecondaryButtonText = "Discard recovery", CloseButtonText = "Keep pending", DefaultButton = ContentDialogButton.Close };
            var result = await dialog.ShowAsync();
            if (!ReferenceEquals(model, ViewModel) || !target.IsCurrent) return;
            if (result == ContentDialogResult.Primary) await model.RetryPendingDeliveryAsync(target);
            else if (result == ContentDialogResult.Secondary) await model.DiscardPendingDeliveryAsync(target);
        }
        catch (OperationCanceledException) { }
        catch (Exception ex) { DesktopLog.Error("Pending delivery review unavailable", ex); if (ReferenceEquals(model, ViewModel)) AccountStatus.Text = "Could not review pending deliveries. Try again."; }
        finally { dialogOpen = false; KeepComposerFocus(); }
    }
}
