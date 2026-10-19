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
    private NativeFileDrop? fileDrop;
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
        postbox.Dispose();
        postbox = new PostboxClient(new HttpClient(new AttachmentFixtureHandler(preview)), new AttachmentFixtureTokens());
        ViewModel = new InboxViewModel(preview, postbox);
#endif
        InitializeComponent();
        MessageList.AddHandler(UIElement.PointerWheelChangedEvent, new PointerEventHandler((_, _) => scrollVersion++), true);
        MessageList.AddHandler(UIElement.PointerPressedEvent, new PointerEventHandler((_, _) => scrollVersion++), true);
        Title = BuildLabel;
        InitializeTheme();
        AppWindow.Resize(new SizeInt32(1100, 720));
        AppWindow.SetIcon(Path.Combine(AppContext.BaseDirectory, "Assets", "Pigeonpost.ico"));
        AppWindow.Changed += Window_Changed;
        ViewModel.PropertyChanged += ViewModel_PropertyChanged;
        ViewModel.MessagesUpdating += Messages_Updating;
        ViewModel.MessagesUpdated += Messages_Updated;
        ViewModel.InboxUpdated += Inbox_Updated;
        Closed += (_, _) =>
        {
            fileDrop?.Dispose();
            RemoveCallView();
            CloseViewer();
            lifetime.Cancel();
            signInAttempt?.Cancel();
            refreshTimer?.Stop();
            notifications?.Dispose();
            accountEvents?.Cancel();
            ViewModel.PropertyChanged -= ViewModel_PropertyChanged;
            ViewModel.MessagesUpdating -= Messages_Updating;
            ViewModel.MessagesUpdated -= Messages_Updated;
            ViewModel.InboxUpdated -= Inbox_Updated;
            ViewModel.Dispose();
        };
    }

    private async void Root_Loaded(object sender, RoutedEventArgs e)
    {
        if (initialized) return;
        initialized = true;
        UpdateLayoutMode();
        var window = WinRT.Interop.WindowNative.GetWindowHandle(this);
        fileDrop = new NativeFileDrop(window, (x, y) =>
        {
            if (dialogOpen || !ViewModel.CanCompose || MessagingArea.Visibility != Visibility.Visible) return false;
            var origin = new DropPoint();
            if (!NativeFileDrop.ClientToScreen(window, ref origin)) return false;
            var point = MessagingArea.TransformToVisual(Root).TransformPoint(new Windows.Foundation.Point());
            var scale = Root.XamlRoot.RasterizationScale;
            return x >= origin.X + point.X * scale && x < origin.X + (point.X + MessagingArea.ActualWidth) * scale
                && y >= origin.Y + point.Y * scale && y < origin.Y + (point.Y + MessagingArea.ActualHeight) * scale;
        }, paths => _ = AttachFilesAsync(async () =>
        {
            var items = new List<Windows.Storage.IStorageItem>();
            foreach (var path in paths) items.Add(await Windows.Storage.StorageFile.GetFileFromPathAsync(path));
            return items;
        }));
        DesktopLog.Write("Window loaded");
        _ = RefreshStartupToggleAsync();
        await EnsureStartedAsync();
    }

    // Account work starts once: when the window first loads, or earlier, without any window,
    // when Windows starts Pigeonpost at sign-in.
    private Task? started;
    private bool startedInNotificationArea;
    private Task EnsureStartedAsync() => started ??= StartAccountAsync();

    private async Task StartAccountAsync()
    {
#if UI_TESTS
        // Needs a rendered window. The call test runs in the runner's own (administrator) session,
        // where the shell cannot thumbnail video; the standard-user smoke covers previews.
        if (!startedInNotificationArea && Environment.GetEnvironmentVariable("PIGEONPOST_CALLS_PAGE") is null)
        { await CheckNativePreviewsAsync(); await CheckViewerContentAsync(); }
        await OpenInboxAsync();
        if (Environment.GetEnvironmentVariable("PIGEONPOST_NOTIFICATION_TEST_RESULT") is { } notificationResult)
        {
            await RunNotificationTestsAsync(notificationResult);
            return;
        }
        refreshTimer!.Interval = TimeSpan.FromMilliseconds(400);
        AccountStatus.Text = "Demonstration account";
#else
        await RestoreAccountAsync();
#endif
    }

    public void StartInNotificationArea()
    {
        if (notifications?.CanHide != true)
        {
            DesktopLog.Write("Started at sign-in without a notification-area icon; showing the window");
            ShowInbox();
            return;
        }
        DesktopLog.Write("Started at sign-in; staying in the notification area");
        startedInNotificationArea = true;
        _ = EnsureStartedAsync();
    }

    private void Window_Changed(AppWindow sender, AppWindowChangedEventArgs args)
    {
        var scale = Root.XamlRoot?.RasterizationScale ?? 1;
        var minimumWidth = (int)Math.Ceiling(420 * scale);
        var minimumHeight = (int)Math.Ceiling(500 * scale);
        if (args.DidSizeChange && sender.Presenter is OverlappedPresenter { State: OverlappedPresenterState.Restored }
            && (sender.Size.Width < minimumWidth || sender.Size.Height < minimumHeight))
            sender.Resize(new SizeInt32(Math.Max(minimumWidth, sender.Size.Width), Math.Max(minimumHeight, sender.Size.Height)));
    }

    private async void Mailbox_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (sender is ComboBox { SelectedItem: Mailbox mailbox } && mailbox.Address != ViewModel.SelectedMailbox?.Address)
        {
            await ViewModel.SwitchMailboxAsync(mailbox);
            await AcknowledgeVisibleAsync();
        }
    }

    private async void Conversation_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (!ViewModel.IsUpdatingLists && sender is ListView { SelectedItem: Conversation conversation } && conversation.Peer != ViewModel.SelectedConversation?.Peer)
        {
            ViewModel.SelectConversation(conversation);
            ShowConversation();
            await AcknowledgeVisibleAsync();
        }
    }

    private async void Subject_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        var subject = sender switch { ListView list => list.SelectedItem as Subject, ComboBox box => box.SelectedItem as Subject, _ => null };
        if (!ViewModel.IsUpdatingLists && subject is not null)
        {
            ViewModel.SelectSubject(subject);
            await AcknowledgeVisibleAsync();
        }
    }

    private async Task SendComposerAsync()
    {
        var context = ViewModel.ContextVersion;
        await ViewModel.SendDraftAsync();
        if (context == ViewModel.ContextVersion && !ViewModel.HasError && ViewModel.Messages.LastOrDefault() is { } last) QueueScroll(last);
        KeepComposerFocus();
    }

    // Sending briefly disables the composer (and a click moves focus to Send), so put the caret
    // back for the next message, unless the person has moved on to another field meanwhile.
    private void KeepComposerFocus() => DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () =>
    {
        if (dialogOpen || !ViewModel.CanCompose || MessagingArea.Visibility != Visibility.Visible || callMode == CallMode.Full) return;
        if (FocusManager.GetFocusedElement(Root.XamlRoot) is TextBox other && !ReferenceEquals(other, Composer)) return;
        Composer.Focus(FocusState.Programmatic);
    });
    private async void Send_Click(object sender, RoutedEventArgs e) => await SendComposerAsync();
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
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = "New conversation", Content = panel,
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
            if (await dialog.ShowAsync() == ContentDialogResult.Primary)
            {
                await ViewModel.StartConversationAsync(peer.Text, message.Text);
                ShowConversation();
            }
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
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = "New subject", Content = panel,
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

    // Dialogs open in a popup outside Root, so they take the window's theme and the shared
    // control styling explicitly.
    private static ResourceDictionary DialogTheme()
    {
        var resources = new ResourceDictionary();
        resources.MergedDictionaries.Add(new ResourceDictionary { Source = new Uri("ms-appx:///Themes/Controls.xaml") });
        return resources;
    }

    private void Find_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args) { ShowFind(); args.Handled = true; }
    private void SenderSearch_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        compactConversation = false;
        UpdateLayoutMode();
        SenderSearchBox.Focus(FocusState.Programmatic);
        args.Handled = true;
    }
    private async void NewConversation_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args) { args.Handled = true; await NewConversationAsync(); }
    private async void Refresh_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args) { args.Handled = true; await ViewModel.RefreshAsync(); }
    private async void Send_Invoked(KeyboardAccelerator sender, KeyboardAcceleratorInvokedEventArgs args)
    {
        if (!ReferenceEquals(FocusManager.GetFocusedElement(Root.XamlRoot), Composer) || ViewModel.IsSending) return;
        args.Handled = true;
        await SendComposerAsync();
    }

    // Ctrl+Enter sends from the composer itself: a multi-line text box would otherwise take the
    // Enter as a new line before the window's accelerator sees it.
    private async void Composer_PreviewKeyDown(object sender, KeyRoutedEventArgs e)
    {
        if (e.Key != Windows.System.VirtualKey.Enter || !Microsoft.UI.Input.InputKeyboardSource.GetKeyStateForCurrentThread(Windows.System.VirtualKey.Control)
            .HasFlag(Windows.UI.Core.CoreVirtualKeyStates.Down)) return;
        e.Handled = true;
        await SendComposerAsync();
    }

    private void CopyMessage_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: string text }) CopyText(text);
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
            var dialog = new ContentDialog { XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = $"Delete {ViewModel.SubjectTitle}?",
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
        TraceScroll($"queued request={request} context={context} messages={model.Messages.Count} leading={leading}");
        DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, async () =>
        {
            // A virtualized list revises its estimated extent as rows are realized. Settle the
            // requested tail over a few frames; context changes and user input cancel the work.
            for (var attempt = 0; attempt < 8; attempt++)
            {
                if (lifetime.IsCancellationRequested || !ReferenceEquals(model, ViewModel) || context != model.ContextVersion || request != scrollVersion)
                { TraceScroll($"cancelled request={request} current={scrollVersion}"); return; }
                var current = model.Messages.FirstOrDefault(m => m.Id == message.Id);
                if (current is null) return;
                MessageList.UpdateLayout();
                if (select) MessageList.SelectedItem = current;
                MessageList.ScrollIntoView(current, leading ? ScrollIntoViewAlignment.Leading : ScrollIntoViewAlignment.Default);
                if (leading || current.Id != model.Messages.LastOrDefault()?.Id) return;
                if (Descendant<ScrollViewer>(MessageList) is { } viewer)
                {
                    viewer.ChangeView(null, viewer.ScrollableHeight, null, disableAnimation: true);
                    TraceScroll($"applied request={request} attempt={attempt} items={MessageList.Items.Count} height={viewer.ScrollableHeight} offset={viewer.VerticalOffset}");
                }
                await Task.Delay(32);
            }
        });
    }

    [System.Diagnostics.Conditional("UI_TESTS")]
    private void TraceScroll(string note)
    {
#if UI_TESTS
        if (Environment.GetEnvironmentVariable("PIGEONPOST_UI_SCROLL_LOG") is { } path)
            File.AppendAllText(path, note + Environment.NewLine);
#endif
    }

    private void ViewModel_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (e.PropertyName == nameof(InboxViewModel.HasConversation)) UpdateLayoutMode();
        if (e.PropertyName is nameof(InboxViewModel.SelectedConversation) or nameof(InboxViewModel.HasConversation)) UpdateCallButtons();
        if (e.PropertyName == nameof(InboxViewModel.SelectedMailbox)) _ = ConfigureCallsAsync();
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
        TraceScroll($"updating context={ViewModel.ContextVersion} previous={messageContext} follow={followMessages} height={viewer?.ScrollableHeight} offset={viewer?.VerticalOffset}");
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
        UpdateBodyMenu(body);
        HighlightBody(body);
    }

    private void MessageBody_Unloaded(object sender, RoutedEventArgs e)
    {
        if (sender is TextBlock body && messageBodies.Remove(body, out var token))
            body.UnregisterPropertyChangedCallback(TextBlock.TextProperty, token);
    }

    // Selectable text would otherwise open its built-in Select All menu on every right-click,
    // hiding the message menu. With a selection, right-click offers the standard text commands
    // (Copy); without one, the card's own menu (Copy original message, Save attachments).
    private TextCommandBarFlyout? selectionMenu;
    private void MessageBody_SelectionChanged(object sender, RoutedEventArgs e)
    {
        if (sender is TextBlock body) UpdateBodyMenu(body);
    }

    private void UpdateBodyMenu(TextBlock body)
    {
        if (body.SelectedText.Length > 0) { body.ContextFlyout = selectionMenu ??= new TextCommandBarFlyout(); return; }
        for (var node = VisualTreeHelper.GetParent(body); node is not null; node = VisualTreeHelper.GetParent(node))
            if (node is FrameworkElement { ContextFlyout: { } menu }) { body.ContextFlyout = menu; return; }
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

public sealed class BooleanVisibilityConverter : Microsoft.UI.Xaml.Data.IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language) => value is true ? Visibility.Visible : Visibility.Collapsed;
    public object ConvertBack(object value, Type targetType, object parameter, string language) => throw new NotSupportedException();
}

public sealed class TextVisibilityConverter : Microsoft.UI.Xaml.Data.IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language) => value is string { Length: > 0 } ? Visibility.Visible : Visibility.Collapsed;
    public object ConvertBack(object value, Type targetType, object parameter, string language) => throw new NotSupportedException();
}

public sealed class MessageTemplateSelector : DataTemplateSelector
{
    public DataTemplate? Incoming { get; set; }
    public DataTemplate? Outgoing { get; set; }
    protected override DataTemplate SelectTemplateCore(object item) => item is ThreadMessage { IsOutgoing: true } ? Outgoing! : Incoming!;
    protected override DataTemplate SelectTemplateCore(object item, DependencyObject container) => SelectTemplateCore(item);
}
