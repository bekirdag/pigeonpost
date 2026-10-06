using System.ComponentModel;
using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Controls.Primitives;
using Microsoft.UI.Xaml.Documents;
using Microsoft.UI.Xaml.Input;
using Microsoft.UI.Xaml.Media;
using Pigeonpost.Core;
using Windows.ApplicationModel.DataTransfer;
using Windows.Graphics;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow : Window
{
    public InboxViewModel ViewModel { get; private set; }
    public Visibility ToVisibility(bool visible) => visible ? Visibility.Visible : Visibility.Collapsed;
    private bool initialized;
    private bool dialogOpen;
    private int messageContext = -1;
    private int scrollVersion;
    private bool followMessages;
    private readonly Dictionary<TextBlock, long> messageBodies = [];
#if UI_TESTS
    private readonly PreviewInboxService preview;
#endif
    public string BuildLabel => "Pigeonpost " + (System.Reflection.Assembly.GetExecutingAssembly().GetName().Version?.ToString(3) ?? "1.0.0");

    public MainWindow()
    {
        session = new AccountSession(http, new WindowsTokenStore());
        postbox = new PostboxClient(http, session, transfers: transfers);
        ViewModel = new InboxViewModel(postbox);
#if UI_TESTS
        // Compiled only in the separate UI-test binary; Store packages never enable fixtures.
        var vaultTest = new WindowsTokenStore();
        vaultTest.Save("ui-test-only-session");
        if (vaultTest.Load() != "ui-test-only-session") throw new InvalidOperationException("Native vault round trip failed.");
        vaultTest.Clear();
        if (vaultTest.Load() is not null) throw new InvalidOperationException("Native vault clearing failed.");
        ViewModel.Dispose();
        preview = new PreviewInboxService(longHistory: true);
        ViewModel = new InboxViewModel(preview);
        postbox.Dispose();
        postbox = new PostboxClient(new HttpClient(new AttachmentFixtureHandler(preview)), new AttachmentFixtureTokens());
#endif
        InitializeComponent();
        Title = BuildLabel;
        AppWindow.Resize(new SizeInt32(1100, 720));
        AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "Assets", "Pigeonpost.ico"));
        AppWindow.Changed += Window_Changed;
        ViewModel.PropertyChanged += ViewModel_PropertyChanged;
        ViewModel.MessagesUpdating += Messages_Updating;
        ViewModel.MessagesUpdated += Messages_Updated;
        Closed += (_, _) =>
        {
            lifetime.Cancel();
            signInAttempt?.Cancel();
            refreshTimer?.Stop();
            ViewModel.PropertyChanged -= ViewModel_PropertyChanged;
            ViewModel.MessagesUpdating -= Messages_Updating;
            ViewModel.MessagesUpdated -= Messages_Updated;
            ViewModel.Dispose();
        };
    }

    private async void Root_Loaded(object sender, RoutedEventArgs e)
    {
        if (initialized) return;
        initialized = true;
#if UI_TESTS
        await OpenInboxAsync();
        refreshTimer!.Interval = TimeSpan.FromMilliseconds(400);
        AccountStatus.Text = "Demonstration account";
#else
        await RestoreAccountAsync();
#endif
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
        if (!ViewModel.IsUpdatingLists && sender is ListView { SelectedItem: Conversation conversation } && conversation.Peer != ViewModel.SelectedConversation?.Peer)
        {
            ViewModel.SelectConversation(conversation);
            await ViewModel.AcknowledgeSelectedAsync();
        }
    }

    private void Subject_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (!ViewModel.IsUpdatingLists && sender is ListView { SelectedItem: Subject subject }) ViewModel.SelectSubject(subject);
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
        if (dialogOpen || ViewModel.SelectedMailbox is null || ViewModel.IsBusy || ViewModel.IsSending) return;
        dialogOpen = true;
        try
        {
            var peer = new TextBox { Header = "Pigeonpost address", PlaceholderText = "/bekir", Text = "/", MaxLength = 512 };
            var message = new TextBox { Header = "First message", AcceptsReturn = true, TextWrapping = TextWrapping.Wrap, MinHeight = 90, MaxLength = 20000 };
            var panel = new StackPanel { Spacing = 16 };
            panel.Children.Add(peer);
            panel.Children.Add(message);
            panel.Children.Add(new TextBlock { Text = "Leave the message empty to open the conversation.", TextWrapping = TextWrapping.Wrap });
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(peer, "Pigeonpost address");
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(message, "First message");
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, Title = "New conversation", Content = panel,
                PrimaryButtonText = "Open", CloseButtonText = "Cancel", IsPrimaryButtonEnabled = false
            };
            void Validate(object _, TextChangedEventArgs __)
            {
                dialog.IsPrimaryButtonEnabled = PostAddress.IsValid(peer.Text);
                dialog.PrimaryButtonText = string.IsNullOrWhiteSpace(message.Text) ? "Open" : "Send";
            }
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
            Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(title, "Subject title");
            var error = new TextBlock { TextWrapping = TextWrapping.Wrap };
            var panel = new StackPanel { Spacing = 12 };
            panel.Children.Add(title);
            panel.Children.Add(error);
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, Title = "New subject", Content = panel,
                PrimaryButtonText = "Create", CloseButtonText = "Cancel", IsPrimaryButtonEnabled = false
            };
            title.TextChanged += (_, _) => dialog.IsPrimaryButtonEnabled = !string.IsNullOrWhiteSpace(title.Text);
            dialog.PrimaryButtonClick += async (_, args) =>
            {
                var deferral = args.GetDeferral();
                dialog.IsPrimaryButtonEnabled = false;
                try
                {
                    args.Cancel = !await ViewModel.CreateSubjectAsync(title.Text);
                    if (args.Cancel) error.Text = ViewModel.Error ?? "Could not create this subject. Please try again.";
                }
                finally { dialog.IsPrimaryButtonEnabled = true; deferral.Complete(); }
            };
            await dialog.ShowAsync();
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
        CopyText(body);
    }

    private void CopyPeer_Click(object sender, RoutedEventArgs e)
    {
        var peer = (sender as MenuFlyoutItem)?.Tag as string ?? ViewModel.SelectedConversation?.Peer;
        if (peer is not null) CopyText(peer);
    }

    private async void DeleteSubject_Click(object sender, RoutedEventArgs e)
    {
        if (dialogOpen || !ViewModel.CanDeleteSubject) return;
        dialogOpen = true;
        try
        {
            var dialog = new ContentDialog { XamlRoot = Root.XamlRoot, Title = $"Delete {ViewModel.SubjectTitle}?",
                Content = "This deletes the subject and its messages from this mailbox. The other side keeps its copy.",
                PrimaryButtonText = "Delete subject", CloseButtonText = "Cancel" };
            if (await dialog.ShowAsync() == ContentDialogResult.Primary) await ViewModel.DeleteSubjectAsync();
        }
        finally { dialogOpen = false; }
    }

    private void QueueScroll(ThreadMessage message, bool leading = false, bool select = false)
    {
        var model = ViewModel;
        var context = model.ContextVersion;
        var request = ++scrollVersion;
        DispatcherQueue.TryEnqueue(() =>
        {
            if (lifetime.IsCancellationRequested || !ReferenceEquals(model, ViewModel) || context != model.ContextVersion || request != scrollVersion) return;
            var current = model.Messages.FirstOrDefault(m => m.Id == message.Id);
            if (current is null) return;
            if (select) MessageList.SelectedItem = current;
            MessageList.ScrollIntoView(current, leading ? ScrollIntoViewAlignment.Leading : ScrollIntoViewAlignment.Default);
        });
    }

    private void ViewModel_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(InboxViewModel.Find))
            foreach (var body in messageBodies.Keys) HighlightBody(body);
        if (e.PropertyName == nameof(InboxViewModel.CurrentMatch) && ViewModel.CurrentMatch is { } match)
            QueueScroll(match, leading: true, select: true);
    }

    private void Messages_Updating(object? sender, EventArgs e)
    {
        // Decide before the collection changes: appends must not pull a reader out of history.
        var viewer = Descendant<ScrollViewer>(MessageList);
        followMessages = ViewModel.ContextVersion != messageContext || viewer is null
            || viewer.ScrollableHeight - viewer.VerticalOffset < 48;
        messageContext = ViewModel.ContextVersion;
    }

    private void Messages_Updated(object? sender, EventArgs e)
    {
        if (followMessages && ViewModel.Find.Length == 0 && ViewModel.Messages.LastOrDefault() is { } last)
            QueueScroll(last);
    }

    private void MessageBody_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is not TextBlock body) return;
        if (!messageBodies.ContainsKey(body))
            messageBodies[body] = body.RegisterPropertyChangedCallback(TextBlock.TextProperty, (sender, _) => HighlightBody((TextBlock)sender));
        HighlightBody(body);
    }

    private void MessageBody_Unloaded(object sender, RoutedEventArgs e)
    {
        if (sender is TextBlock body && messageBodies.Remove(body, out var token))
            body.UnregisterPropertyChangedCallback(TextBlock.TextProperty, token);
    }

    private void HighlightBody(TextBlock body)
    {
        body.TextHighlighters.Clear();
        var query = ViewModel.Find.Trim();
        if (query.Length == 0) return;
        var highlight = new TextHighlighter
        {
            Background = new SolidColorBrush(Microsoft.UI.Colors.Gold),
            Foreground = new SolidColorBrush(Microsoft.UI.Colors.Black)
        };
        for (var start = 0; start <= body.Text.Length - query.Length;)
        {
            var index = body.Text.IndexOf(query, start, StringComparison.OrdinalIgnoreCase);
            if (index < 0) break;
            highlight.Ranges.Add(new TextRange { StartIndex = index, Length = query.Length });
            start = index + query.Length;
        }
        if (highlight.Ranges.Count > 0) body.TextHighlighters.Add(highlight);
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
