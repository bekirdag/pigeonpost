using System.ComponentModel;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Pigeonpost.Core;
using Windows.ApplicationModel.DataTransfer;
using Windows.Graphics;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow : Window
{
    public InboxViewModel ViewModel { get; } = new(new PreviewInboxService());
    public Visibility ToVisibility(bool visible) => visible ? Visibility.Visible : Visibility.Collapsed;
    private bool initialized;
    private bool dialogOpen;
    private string? messageContext;

    public MainWindow()
    {
        InitializeComponent();
        AppWindow.Resize(new SizeInt32(1100, 720));
        AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "Assets", "Pigeonpost.ico"));
        AppWindow.Changed += Window_Changed;
        ViewModel.PropertyChanged += ViewModel_PropertyChanged;
        Closed += (_, _) =>
        {
            ViewModel.PropertyChanged -= ViewModel_PropertyChanged;
            ViewModel.Dispose();
        };
    }

    private async void Root_Loaded(object sender, RoutedEventArgs e)
    {
        if (initialized) return;
        initialized = true;
        await ViewModel.InitializeAsync();
        await ViewModel.AcknowledgeSelectedAsync();
    }

    private void Window_Changed(AppWindow sender, AppWindowChangedEventArgs args)
    {
        if (args.DidSizeChange && sender.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Restored }
            && (sender.Size.Width < 900 || sender.Size.Height < 560))
            sender.Resize(new SizeInt32(Math.Max(900, sender.Size.Width), Math.Max(560, sender.Size.Height)));
    }

    private async void Mailbox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (sender is ComboBox { SelectedItem: Mailbox mailbox } && mailbox.Address != ViewModel.SelectedMailbox?.Address)
        {
            await ViewModel.SwitchMailboxAsync(mailbox);
            await ViewModel.AcknowledgeSelectedAsync();
        }
    }

    private async void Conversation_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (sender is ListView { SelectedItem: Conversation conversation } && conversation.Peer != ViewModel.SelectedConversation?.Peer)
        {
            ViewModel.SelectConversation(conversation);
            await ViewModel.AcknowledgeSelectedAsync();
        }
    }

    private void Subject_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (sender is ListView { SelectedItem: Subject subject }) ViewModel.SelectSubject(subject);
    }

    private async void Send_Click(object sender, RoutedEventArgs e) => await ViewModel.SendDraftAsync();
    private async void Refresh_Click(object sender, RoutedEventArgs e) => await ViewModel.RefreshAsync();
    private async void Archive_Click(object sender, RoutedEventArgs e) => await ViewModel.ArchiveSelectedAsync();
    private void PreviousMatch_Click(object sender, RoutedEventArgs e) => ViewModel.MoveMatch(-1);
    private void NextMatch_Click(object sender, RoutedEventArgs e) => ViewModel.MoveMatch(1);
    private async void NewConversation_Click(object sender, RoutedEventArgs e) => await NewConversationAsync();
    private async void NewSubject_Click(object sender, RoutedEventArgs e) => await NewSubjectAsync();

    private async Task NewConversationAsync()
    {
        if (dialogOpen || ViewModel.SelectedMailbox is null || ViewModel.IsBusy) return;
        dialogOpen = true;
        try
        {
            var peer = new TextBox { Header = "Pigeonpost address", PlaceholderText = "/bekir", Text = "/", MaxLength = 512 };
            var message = new TextBox { Header = "First message", AcceptsReturn = true, TextWrapping = TextWrapping.Wrap, MinHeight = 90, MaxLength = 20000 };
            var panel = new StackPanel { Spacing = 16 };
            panel.Children.Add(peer);
            panel.Children.Add(message);
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, Title = "New conversation", Content = panel,
                PrimaryButtonText = "Start conversation", CloseButtonText = "Cancel", IsPrimaryButtonEnabled = false
            };
            void Validate(object _, TextChangedEventArgs __) => dialog.IsPrimaryButtonEnabled = PostAddress.IsValid(peer.Text) && !string.IsNullOrWhiteSpace(message.Text);
            peer.TextChanged += (_, _) =>
            {
                var value = PostAddress.Input(peer.Text);
                if (value == peer.Text) return;
                var position = peer.SelectionStart;
                peer.Text = value;
                peer.SelectionStart = Math.Clamp(position + 1, 1, value.Length);
            };
            peer.TextChanged += Validate;
            message.TextChanged += Validate;
            if (await dialog.ShowAsync() == ContentDialogResult.Primary) await ViewModel.StartConversationAsync(peer.Text, message.Text);
        }
        finally { dialogOpen = false; }
    }

    private async Task NewSubjectAsync()
    {
        if (dialogOpen || !ViewModel.CanCompose) return;
        dialogOpen = true;
        try
        {
            var title = new TextBox { Header = "Subject", PlaceholderText = "What is this conversation about?", MaxLength = 200 };
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, Title = "New subject", Content = title,
                PrimaryButtonText = "Create", CloseButtonText = "Cancel", IsPrimaryButtonEnabled = false
            };
            title.TextChanged += (_, _) => dialog.IsPrimaryButtonEnabled = !string.IsNullOrWhiteSpace(title.Text);
            if (await dialog.ShowAsync() == ContentDialogResult.Primary) await ViewModel.CreateSubjectAsync(title.Text);
        }
        finally { dialogOpen = false; }
    }

    private void SenderColumn_DragDelta(object sender, DragDeltaEventArgs e) => SenderColumn.Width = new GridLength(Math.Clamp(SenderColumn.Width.Value + e.HorizontalChange, 240, 420));
    private void SubjectColumn_DragDelta(object sender, DragDeltaEventArgs e) => SubjectColumn.Width = new GridLength(Math.Clamp(SubjectColumn.Width.Value + e.HorizontalChange, 170, 340));
    private void Find_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args) { FindBox.Focus(FocusState.Programmatic); args.Handled = true; }
    private void SenderSearch_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args) { SenderSearchBox.Focus(FocusState.Programmatic); args.Handled = true; }
    private async void NewConversation_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args) { args.Handled = true; await NewConversationAsync(); }
    private async void Refresh_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args) { args.Handled = true; await ViewModel.RefreshAsync(); }
    private async void Send_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        if (!ReferenceEquals(FocusManager.GetFocusedElement(Root.XamlRoot), Composer)) return;
        args.Handled = true;
        await ViewModel.SendDraftAsync();
    }

    private void CopyOriginal_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not MenuFlyoutItem { Tag: string body }) return;
        var package = new DataPackage();
        package.SetText(body);
        Clipboard.SetContent(package);
    }

    private void ViewModel_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(InboxViewModel.CurrentMatch) && ViewModel.CurrentMatch is { } match)
            DispatcherQueue.TryEnqueue(() => { MessageList.SelectedItem = match; MessageList.ScrollIntoView(match, ScrollIntoViewAlignment.Leading); });
        if (e.PropertyName != nameof(InboxViewModel.Messages)) return;
        var context = $"{ViewModel.SelectedMailbox?.Address}|{ViewModel.SelectedConversation?.Peer}|{ViewModel.SelectedSubject?.Id}";
        var viewer = Descendant<ScrollViewer>(MessageList);
        var follow = context != messageContext || viewer is null || viewer.ScrollableHeight - viewer.VerticalOffset < 48;
        messageContext = context;
        if (follow && ViewModel.Messages.LastOrDefault() is { } last)
            DispatcherQueue.TryEnqueue(() => MessageList.ScrollIntoView(last));
    }

    private static T? Descendant<T>(DependencyObject root) where T : DependencyObject
    {
        for (var i = 0; i < VisualTreeHelper.GetChildrenCount(root); i++)
        {
            var child = VisualTreeHelper.GetChild(root, i);
            if (child is T found) return found;
            if (Descendant<T>(child) is { } nested) return nested;
        }
        return null;
    }
}

public sealed class MessageTemplateSelector : DataTemplateSelector
{
    public DataTemplate? Incoming { get; set; }
    public DataTemplate? Outgoing { get; set; }
    protected override DataTemplate SelectTemplateCore(object item) => item is ThreadMessage { IsOutgoing: true } ? Outgoing! : Incoming!;
    protected override DataTemplate SelectTemplateCore(object item, DependencyObject container) => SelectTemplateCore(item);
}
