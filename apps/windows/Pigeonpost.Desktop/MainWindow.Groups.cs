using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Pigeonpost.Core;
using Pigeonpost.Core.Groups;

namespace Pigeonpost.Desktop;

//  Phase 3b on Windows: end-to-end encrypted groups (docs/groups.md, SDS §4). The MLS state
//  machine is Pigeonpost.Core's GroupClient over the Rust library shipped beside the app; this file
//  is its user interface: the group list with invitations, the group view with announcement mode,
//  disappearing timers and encrypted files, Group info with device fingerprints and the safety
//  number, and group calls through the WebView2 call host.
public sealed partial class MainWindow
{
    private ListView? groupList;
    private ListView? groupLines;
    private TextBlock? groupTitle, groupSubtitle, groupKeying, groupAnnouncement, groupPlaceholder;
    private InfoBar? groupTimer, groupError, groupStatus;
    private StackPanel? groupInvitation, groupComposer, groupFiles, groupHeaderActions;
    private TextBox? groupDraft;
    private Button? groupSend, groupJoinCall;
    private Grid? groupView;
    private bool groupPageHooked;
    private string? groupDraftSeen;

    private FrameworkElement BuildGroupList()
    {
        var host = new Grid();
        groupList = new ListView
        {
            ItemsSource = groupsModel.Groups, ItemTemplate = (DataTemplate)Root.Resources["GroupRowTemplate"], SelectionMode = ListViewSelectionMode.Single,
            IsItemClickEnabled = true,
        };
        groupList.ItemContainerStyle = (Style)Microsoft.UI.Xaml.Markup.XamlReader.Load(
            "<Style xmlns='http://schemas.microsoft.com/winfx/2006/xaml/presentation' TargetType='ListViewItem'><Setter Property='HorizontalContentAlignment' Value='Stretch' /><Setter Property='Padding' Value='10,10' /><Setter Property='Margin' Value='4,1' /></Style>");
        AutomationProperties.SetName(groupList, "Groups list");
        groupList.ItemClick += async (_, e) =>
        {
            if (e.ClickedItem is not GroupRow row) return;
            compactConversation = true;
            await groupsModel.SelectAsync(row);
            UpdateLayoutMode();
            UpdateGroupPage();
        };
        host.Children.Add(groupList);
        var empty = new TextBlock { Text = groupsModel.EmptyText, Margin = new Thickness(16, 24, 16, 24), TextWrapping = TextWrapping.Wrap, FontSize = 13, IsHitTestVisible = false };
        groupsModel.Groups.CollectionChanged += (_, _) => { empty.Text = groupsModel.EmptyText; empty.Visibility = ToVisibility(empty.Text.Length > 0); };
        empty.Visibility = ToVisibility(empty.Text.Length > 0);
        host.Children.Add(empty);
        return host;
    }

    private FrameworkElement BuildGroupPage()
    {
        var root = new Grid();
        groupView = new Grid();
        groupView.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        groupView.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        groupView.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        groupView.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });

        // Header: name, members, call buttons, "Call in progress — Join", Group info.
        var header = new Grid { Padding = new Thickness(24, 12, 20, 12), ColumnSpacing = 10 };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var back = new Button { Content = new SymbolIcon(Symbol.Back), Style = (Style)Root.Resources["QuietIconButton"], Visibility = ToVisibility(IsCompact) };
        AutomationProperties.SetName(back, "Back to groups");
        back.Click += (_, _) => { compactConversation = false; UpdateLayoutMode(); };
        header.Children.Add(back);
        var titles = new StackPanel { Spacing = 1, VerticalAlignment = VerticalAlignment.Center };
        groupTitle = new TextBlock { FontSize = 15, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, TextTrimming = TextTrimming.CharacterEllipsis };
        AutomationProperties.SetHeadingLevel(groupTitle, Microsoft.UI.Xaml.Automation.Peers.AutomationHeadingLevel.Level1);
        groupSubtitle = new TextBlock { FontSize = 12.5, Opacity = 0.75 };
        titles.Children.Add(groupTitle);
        titles.Children.Add(groupSubtitle);
        Grid.SetColumn(titles, 1);
        header.Children.Add(titles);
        groupHeaderActions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, VerticalAlignment = VerticalAlignment.Center };
        groupJoinCall = new Button { Content = "Call in progress — Join", Visibility = Visibility.Collapsed, Style = (Style)Application.Current.Resources["AccentButtonStyle"] };
        AutomationProperties.SetName(groupJoinCall, "Join the group call");
        groupJoinCall.Click += async (_, _) => await GroupCallAsync(video: false, join: true);
        groupHeaderActions.Children.Add(groupJoinCall);
        Button Tool(string glyph, string name, RoutedEventHandler click)
        {
            var button = new Button { Content = new FontIcon { Glyph = glyph, FontSize = 14 }, Style = (Style)Root.Resources["ToolbarIconButton"] };
            AutomationProperties.SetName(button, name);
            ToolTipService.SetToolTip(button, name);
            button.Click += click;
            groupHeaderActions.Children.Add(button);
            return button;
        }
        Tool("", "Group voice call", async (_, _) => await GroupCallAsync(video: false, join: false));
        Tool("", "Group video call", async (_, _) => await GroupCallAsync(video: true, join: false));
        groupHeaderActions.Children.Add(BuildGoLiveButton());
        Tool("", "Group info", async (_, _) => await GroupInfoAsync());
        Grid.SetColumn(groupHeaderActions, 2);
        header.Children.Add(groupHeaderActions);
        groupView.Children.Add(header);

        // Banners: invitation, keying, timer, errors.
        var banners = new StackPanel { Padding = new Thickness(24, 0, 24, 4), Spacing = 6 };
        groupInvitation = new StackPanel { Spacing = 8 };
        groupInvitation.Children.Add(new TextBlock { Text = "You're invited to this group. Accepting shares your mailbox address with its members. You'll see messages sent after you join.", TextWrapping = TextWrapping.Wrap });
        var answers = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        var accept = new Button { Content = "Accept", Style = (Style)Root.Resources["PrimaryActionButton"] };
        AutomationProperties.SetName(accept, "Accept invitation");
        accept.Click += async (_, _) => { await groupsModel.AnswerInvitationAsync(true); UpdateGroupPage(); };
        var decline = new Button { Content = "Decline", Style = (Style)Root.Resources["SecondaryActionButton"] };
        AutomationProperties.SetName(decline, "Decline invitation");
        decline.Click += async (_, _) => { await groupsModel.AnswerInvitationAsync(false); UpdateGroupPage(); };
        answers.Children.Add(accept);
        answers.Children.Add(decline);
        groupInvitation.Children.Add(answers);
        banners.Children.Add(groupInvitation);
        groupKeying = new TextBlock { Text = "Setting up encryption: waiting for a member's device to add this one. This happens automatically when one of them is online.", TextWrapping = TextWrapping.Wrap, FontSize = 13 };
        banners.Children.Add(groupKeying);
        groupTimer = new InfoBar { IsClosable = false, Severity = InfoBarSeverity.Informational };
        AutomationProperties.SetName(groupTimer, "Disappearing messages");
        banners.Children.Add(groupTimer);
        groupError = new InfoBar { IsClosable = true, Severity = InfoBarSeverity.Error };
        banners.Children.Add(groupError);
        groupStatus = new InfoBar { IsClosable = true, Severity = InfoBarSeverity.Success };
        banners.Children.Add(groupStatus);
        BuildLiveControls(banners);
        Grid.SetRow(banners, 1);
        groupView.Children.Add(banners);

        groupLines = new ListView
        {
            ItemsSource = groupsModel.Lines, ItemTemplateSelector = (DataTemplateSelector)Root.Resources["GroupLineSelector"],
            SelectionMode = ListViewSelectionMode.None, Margin = new Thickness(0, 6, 0, 6),
        };
        groupLines.ItemContainerStyle = (Style)Microsoft.UI.Xaml.Markup.XamlReader.Load(
            "<Style xmlns='http://schemas.microsoft.com/winfx/2006/xaml/presentation' TargetType='ListViewItem'><Setter Property='HorizontalContentAlignment' Value='Stretch' /><Setter Property='Padding' Value='0' /></Style>");
        groupLines.ItemsPanel = (ItemsPanelTemplate)Microsoft.UI.Xaml.Markup.XamlReader.Load(
            "<ItemsPanelTemplate xmlns='http://schemas.microsoft.com/winfx/2006/xaml/presentation'><ItemsStackPanel ItemsUpdatingScrollMode='KeepLastItemInView' /></ItemsPanelTemplate>");
        AutomationProperties.SetName(groupLines, "Group messages");
        Grid.SetRow(groupLines, 2);
        groupView.Children.Add(groupLines);

        // Composer, or the announcement bar in its place.
        var bottom = new StackPanel { Padding = new Thickness(24, 10, 24, 16), Spacing = 6 };
        groupAnnouncement = new TextBlock { TextAlignment = TextAlignment.Center, Margin = new Thickness(0, 8, 0, 8), Opacity = 0.8 };
        AutomationProperties.SetName(groupAnnouncement, "Posting restricted");
        bottom.Children.Add(groupAnnouncement);
        groupComposer = new StackPanel { Spacing = 6 };
        groupComposer.Children.Add(BuildGroupModeBar());
        groupFiles = new StackPanel { Spacing = 4 };
        groupComposer.Children.Add(groupFiles);
        groupDraft = new TextBox { PlaceholderText = "Write to the group…", AcceptsReturn = true, TextWrapping = TextWrapping.Wrap, MinHeight = 56, MaxHeight = 144, MaxLength = 20000 };
        AutomationProperties.SetName(groupDraft, "Group message");
        groupDraftSeen = null; // a new box shows the model's draft once
        groupDraft.TextChanged += (_, _) => { groupsModel.Draft = groupDraft.Text; groupDraftSeen = groupsModel.Draft; groupSend!.IsEnabled = groupsModel.CanSend; };
        groupDraft.PreviewKeyDown += async (_, e) =>
        {
            // Escape leaves a reply or an edit; an edit gives the box its earlier draft back.
            if (e.Key == Windows.System.VirtualKey.Escape && groupsModel.HasComposerMode)
            {
                e.Handled = true;
                if (groupsModel.CancelComposerMode()) UpdateGroupPage();
                return;
            }
            if (e.Key != Windows.System.VirtualKey.Enter || !Microsoft.UI.Input.InputKeyboardSource.GetKeyStateForCurrentThread(Windows.System.VirtualKey.Control)
                .HasFlag(Windows.UI.Core.CoreVirtualKeyStates.Down)) return;
            e.Handled = true;
            await SendGroupAsync();
        };
        groupComposer.Children.Add(groupDraft);
        var row = new Grid { ColumnSpacing = 8 };
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var attach = groupAttach = new Button { Content = new SymbolIcon(Symbol.Attach), Style = (Style)Root.Resources["QuietIconButton"] };
        AutomationProperties.SetName(attach, "Attach encrypted files…");
        ToolTipService.SetToolTip(attach, "Files are encrypted on this device before upload");
        attach.Click += async (_, _) => await AttachGroupFilesAsync();
        row.Children.Add(attach);
        var hint = new TextBlock { Text = "End-to-end encrypted · Ctrl+Enter to send", FontSize = 12, Opacity = 0.7, VerticalAlignment = VerticalAlignment.Center };
        Grid.SetColumn(hint, 1);
        row.Children.Add(hint);
        groupSend = new Button { Content = "Send", Style = (Style)Root.Resources["PrimaryActionButton"], IsEnabled = false };
        AutomationProperties.SetName(groupSend, "Send to group");
        groupSend.Click += async (_, _) => await SendGroupAsync();
        Grid.SetColumn(groupSend, 2);
        row.Children.Add(groupSend);
        groupComposer.Children.Add(row);
        bottom.Children.Add(groupComposer);
        Grid.SetRow(bottom, 3);
        groupView.Children.Add(bottom);
        root.Children.Add(groupView);

        groupPlaceholder = new TextBlock
        {
            HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center, TextAlignment = TextAlignment.Center,
            TextWrapping = TextWrapping.Wrap, MaxWidth = 440, Margin = new Thickness(24), Opacity = 0.85,
        };
        root.Children.Add(groupPlaceholder);

        if (!groupPageHooked)
        {
            groupPageHooked = true;
            groupsModel.PropertyChanged += (_, _) => UpdateGroupPage();
            groupsModel.DraftFiles.CollectionChanged += (_, _) => UpdateGroupFiles();
        }
        UpdateGroupPage();
        return root;
    }

    private void UpdateGroupPage()
    {
        if (groupView is null || groupPlaceholder is null || sideView != SideView.Groups) return;
        var m = groupsModel;
        groupView.Visibility = ToVisibility(m.HasSelection);
        groupPlaceholder.Visibility = ToVisibility(!m.HasSelection);
        groupPlaceholder.Text = !m.Enabled ? "Groups aren't available for this mailbox."
            : "Select a group, or start one with New group.\n\nGroup messages and files are end-to-end encrypted: only members' devices can read them."
              + (m.DeviceFingerprint is { } fp ? $"\n\nThis device's fingerprint: {fp}" : "");
        groupTitle!.Text = m.Title;
        groupSubtitle!.Text = m.Subtitle;
        groupHeaderActions!.Visibility = ToVisibility(m.CanCall);
        groupInvitation!.Visibility = ToVisibility(m.ShowsInvitation);
        groupKeying!.Visibility = ToVisibility(m.ShowsKeying && !m.ShowsInvitation);
        groupTimer!.IsOpen = m.TimerBanner.Length > 0;
        groupTimer.Message = m.TimerBanner;
        groupError!.IsOpen = m.HasError;
        groupError.Message = m.Error ?? "";
        groupStatus!.IsOpen = !string.IsNullOrEmpty(m.Status);
        groupStatus.Message = m.Status ?? "";
        groupAnnouncement!.Visibility = ToVisibility(m.ShowsAnnouncementBar);
        groupAnnouncement.Text = m.AnnouncementText;
        groupComposer!.Visibility = ToVisibility(m.ShowsComposer);
        groupSend!.IsEnabled = m.CanSend;
        // Only a change the model made itself (sent, cleared, another group, a restored draft) is
        // written into the box: WinUI raises TextChanged asynchronously, so comparing the box with
        // the model mid-typing would overwrite fresh keystrokes and move the caret.
        if (m.Draft != groupDraftSeen)
        {
            groupDraftSeen = m.Draft;
            if (groupDraft!.Text != m.Draft) { groupDraft.Text = m.Draft; groupDraft.SelectionStart = m.Draft.Length; }
        }
        groupDraft.IsEnabled = !m.IsSending;
        UpdateGroupModeBar();
        if (groupList is not null && !ReferenceEquals(groupList.SelectedItem, m.Selected)) groupList.SelectedItem = m.Selected;
        UpdateLiveControls();
        _ = UpdateGroupCallAsync();
        _ = UpdateGroupStreamAsync();
    }

    private void UpdateGroupFiles()
    {
        if (groupFiles is null) return;
        groupFiles.Children.Clear();
        foreach (var file in groupsModel.DraftFiles.ToArray())
        {
            var chip = new Grid { ColumnSpacing = 8 };
            chip.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
            chip.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
            chip.Children.Add(new TextBlock { Text = file.Name, VerticalAlignment = VerticalAlignment.Center, TextTrimming = TextTrimming.CharacterEllipsis });
            var remove = new Button { Content = new FontIcon { Glyph = "", FontSize = 12 }, Style = (Style)Root.Resources["QuietIconButton"] };
            AutomationProperties.SetName(remove, "Remove " + file.Name);
            remove.Click += (_, _) => groupsModel.RemoveFile(file);
            Grid.SetColumn(remove, 1);
            chip.Children.Add(remove);
            groupFiles.Children.Add(chip);
        }
        if (groupSend is not null) groupSend.IsEnabled = groupsModel.CanSend;
    }

    private void ScrollGroupToEnd()
    {
        if (groupLines is null || groupsModel.Lines.LastOrDefault() is not { } last) return;
        DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () =>
        {
            try { groupLines.ScrollIntoView(last); } catch (Exception) { }
        });
    }

    private async Task SendGroupAsync()
    {
        if (!groupsModel.CanSend) return;
        await groupsModel.SendAsync();
        UpdateGroupPage();
        KeepGroupComposerFocus();
    }

    // Sending disables the box for a moment (and a click moves focus to Send): put the caret back
    // for the next message, whether or not the send went through, unless the person moved on.
    private void KeepGroupComposerFocus() => DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () =>
    {
        if (dialogOpen || groupDraft is null || groupComposer?.Visibility != Visibility.Visible || callMode == CallMode.Full || !groupDraft.IsEnabled) return;
        if (Microsoft.UI.Xaml.Input.FocusManager.GetFocusedElement(Root.XamlRoot) is TextBox other && !ReferenceEquals(other, groupDraft)) return;
        groupDraft.Focus(FocusState.Programmatic);
    });

    private async Task AttachGroupFilesAsync()
    {
        try
        {
#if UI_TESTS
            var files = new[] { await Windows.Storage.StorageFile.GetFileFromPathAsync(Environment.GetEnvironmentVariable("PIGEONPOST_UI_ATTACHMENT_FILE")
                ?? Path.Combine(AppContext.BaseDirectory, "PreviewFixtures", "preview.txt")) };
#else
            var picker = new Windows.Storage.Pickers.FileOpenPicker();
            picker.FileTypeFilter.Add("*");
            WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(this));
            var files = await picker.PickMultipleFilesAsync();
#endif
            foreach (var file in files)
            {
                var size = (await file.GetBasicPropertiesAsync()).Size;
                if (size is 0 or > PostboxClient.MaxAttachmentBytes) { AccountStatus.Text = $"{file.Name}: choose a file between 1 byte and 25 MB."; continue; }
                var bytes = await ReadAttachmentAsync(file, lifetime.Token);
                groupsModel.AddFile(new GroupClient.OutgoingFile(file.Name, string.IsNullOrEmpty(file.ContentType) ? "application/octet-stream" : file.ContentType, bytes));
            }
        }
        catch (Exception ex) when (ex is not OperationCanceledException) { AccountStatus.Text = "Could not read the selected files. Please try again."; }
    }

    // Group files are decrypted here, then shown in the same viewer as other files or saved.
    private async void GroupFile_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement { Tag: LocalFile file }) return;
        var attachment = new MessageAttachment("group:" + file.Id, file.Name, file.Mime, file.Size);
        if (attachment.CanOpen) { await OpenViewerAsync(attachment, token => LoadGroupFileAsync(file)); return; }
        var bytes = await LoadGroupFileAsync(file);
        var destination = await SaveDestinationAsync(AttachmentViewer.SafeFileName(file.Name), false);
        if (destination is null) return;
        await Windows.Storage.FileIO.WriteBytesAsync(destination, bytes);
        AccountStatus.Text = $"Saved {file.Name}.";
    }

    private async Task<byte[]> LoadGroupFileAsync(LocalFile file) =>
        await groupsModel.OpenFileAsync(file) ?? throw new IOException("The group file could not be decrypted.");

    // ---- New group ---------------------------------------------------------------------------

    private async void NewGroup_Click(object sender, RoutedEventArgs e)
    {
        if (dialogOpen || !groupsModel.Enabled || ViewModel.SelectedMailbox is not { } mailbox) return;
        dialogOpen = true;
        GroupSettings? settings = null;
        string[] invite = [];
        var joinInstead = false;
        try
        {
            var who = new TextBox { Header = "Who", PlaceholderText = "/jane, /acme/ops …  (one or more, separated by commas)", AcceptsReturn = false };
            AutomationProperties.SetName(who, "Group members");
            var name = new TextBox { Header = "Group name", MaxLength = 80 };
            AutomationProperties.SetName(name, "Group name");
            var description = new TextBox { Header = "Description (optional)", MaxLength = 500, AcceptsReturn = true, TextWrapping = TextWrapping.Wrap };
            AutomationProperties.SetName(description, "Group description");
            var join = new ComboBox { Header = "Who can join", MinWidth = 260 };
            join.Items.Add(new ComboBoxItem { Content = "Private — invite only", Tag = "private" });
            join.Items.Add(new ComboBoxItem { Content = "Public — anyone with the link", Tag = "public" });
            join.SelectedIndex = 0;
            AutomationProperties.SetName(join, "Who can join");
            var post = new ComboBox { Header = "Who can post", MinWidth = 260 };
            post.Items.Add(new ComboBoxItem { Content = "Everyone", Tag = "everyone" });
            post.Items.Add(new ComboBoxItem { Content = "Only admins", Tag = "admins" });
            post.SelectedIndex = 0;
            AutomationProperties.SetName(post, "Who can post");
            var panel = new StackPanel { Spacing = 12, MinWidth = 380, MaxWidth = 480 };
            panel.Children.Add(new TextBlock { Text = "Sending as " + mailbox.Key, FontFamily = (FontFamily)Application.Current.Resources["MonoFont"], FontSize = 12.5 });
            panel.Children.Add(who);
            panel.Children.Add(name);
            panel.Children.Add(description);
            panel.Children.Add(join);
            panel.Children.Add(post);
            panel.Children.Add(new TextBlock { Text = "Messages and files are end-to-end encrypted. Members see who is in the group and each device's fingerprint.", TextWrapping = TextWrapping.Wrap, FontSize = 12, Opacity = 0.8 });
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = "New group",
                Content = new ScrollViewer { Content = panel, MaxHeight = 600 }, PrimaryButtonText = "Create", SecondaryButtonText = "Join with a link…",
                CloseButtonText = "Cancel", IsPrimaryButtonEnabled = false,
            };
            name.TextChanged += (_, _) => dialog.IsPrimaryButtonEnabled = name.Text.Trim().Length is >= 1 and <= 80;
            var result = await dialog.ShowAsync();
            if (result == ContentDialogResult.Secondary) { joinInstead = true; return; }
            if (result != ContentDialogResult.Primary) return;
            invite = Addresses(who.Text);
            settings = new GroupSettings(name.Text.Trim(), string.IsNullOrWhiteSpace(description.Text) ? null : description.Text.Trim(),
                (string)((ComboBoxItem)join.SelectedItem).Tag, (string)((ComboBoxItem)post.SelectedItem).Tag);
        }
        finally { dialogOpen = false; }
        if (joinInstead) { await JoinByLinkAsync(); return; }
        if (settings is null) return;
        if (sideView != SideView.Groups) ShowSideView(SideView.Groups);
        AccountStatus.Text = "Creating the group and its keys…";
        var created = await groupsModel.CreateAsync(settings, invite);
        AccountStatus.Text = created is null ? groupsModel.Error ?? "Could not create the group." : $"Created {created.DisplayName}.";
        compactConversation = created is not null;
        UpdateLayoutMode();
        UpdateGroupPage();
        if (created is not null) KeepGroupComposerFocus();
    }

    private static string[] Addresses(string text) => text.Split([',', ';', ' ', '\n', '\r'], StringSplitOptions.RemoveEmptyEntries)
        .Select(PostAddress.Input).Where(PostAddress.IsValid).Distinct(StringComparer.Ordinal).Take(200).ToArray();

    // ---- Group info --------------------------------------------------------------------------

    private async Task GroupInfoAsync()
    {
        if (dialogOpen || groupsModel.Info is not { } info) return;
        dialogOpen = true;
        try
        {
            var (members, safety) = await groupsModel.LoadInfoAsync();
            ContentDialog? dialog = null;
            Func<Task>? pending = null; // each action closes this sheet first, then runs
            var panel = new StackPanel { Spacing = 12, MinWidth = 420, MaxWidth = 560 };
            if (!string.IsNullOrWhiteSpace(info.Description)) panel.Children.Add(new TextBlock { Text = info.Description, TextWrapping = TextWrapping.Wrap });

            // Visibility is the defence against a device added behind members' backs (P-2 §3, §12).
            var security = new StackPanel { Spacing = 4 };
            security.Children.Add(new TextBlock { Text = "Safety number", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
            var number = new TextBlock { Text = safety ?? "Not available until this device is in the group.", FontFamily = (FontFamily)Application.Current.Resources["MonoFont"], IsTextSelectionEnabled = true, TextWrapping = TextWrapping.Wrap };
            AutomationProperties.SetName(number, "Safety number " + (safety ?? "unavailable"));
            security.Children.Add(number);
            security.Children.Add(new TextBlock
            {
                Text = "Compare this number with other members in person or on a call. It changes whenever a device joins or leaves. Pigeonpost runs hosted mailboxes, so a new device you didn't expect is worth asking about.",
                TextWrapping = TextWrapping.Wrap, FontSize = 12, Opacity = 0.8,
            });
            security.Children.Add(new TextBlock { Text = $"This device: {groupsModel.DeviceId} · {groupsModel.DeviceFingerprint}", FontSize = 12, IsTextSelectionEnabled = true, TextWrapping = TextWrapping.Wrap });
            panel.Children.Add(security);

            panel.Children.Add(new TextBlock { Text = $"Members ({members.Count(m => m.State is "active" or "keying")})", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
            foreach (var member in members.Where(m => m.State != "banned")) panel.Children.Add(MemberRow(member));
            panel.Children.Add(new TextBlock { Text = "Blocking a member hides their messages on your devices. It doesn't remove them from the group or hide the group from them.", TextWrapping = TextWrapping.Wrap, FontSize = 12, Opacity = 0.8 });
            var banned = members.Where(m => m.State == "banned").ToArray();
            if (banned.Length > 0)
            {
                panel.Children.Add(new TextBlock { Text = $"Banned ({banned.Length})", FontWeight = Microsoft.UI.Text.FontWeights.SemiBold });
                panel.Children.Add(new TextBlock { Text = "Banned people cannot join again, even with a link, until an admin unbans them.", TextWrapping = TextWrapping.Wrap, FontSize = 12, Opacity = 0.8 });
                foreach (var member in banned) panel.Children.Add(MemberRow(member));
            }

            var actions = new StackPanel { Spacing = 8 };
            if (info.CanInvite) actions.Children.Add(Action("Add people…", async () => { dialogOpen = false; await InviteToGroupAsync(); }));
            if (GroupAdmin.ManagesLink(info))
            {
                actions.Children.Add(new TextBlock { Text = info.HasLink == true ? "Anyone with the current invite link can join. Resetting makes the old link stop working."
                    : "Create a link anyone can use to join this public group.", TextWrapping = TextWrapping.Wrap, FontSize = 12, Opacity = 0.8 });
                actions.Children.Add(Action(info.HasLink == true ? "Reset invite link" : "Create invite link", async () =>
                {
                    dialogOpen = false;
                    if (await groupsModel.SetLinkAsync(true) is { Url: { Length: > 0 } url }) await ShowGroupLinkAsync(url);
                }));
                if (info.HasLink == true) actions.Children.Add(Action("Turn off invite link", async () => { await groupsModel.SetLinkAsync(false); }));
            }
            if (info.CanChangeTimer) actions.Children.Add(Action("Disappearing messages: " + Timers.Label(info.EphemeralSecs), async () =>
            {
                dialogOpen = false;
                var choice = await ChooseTimerAsync(info.EphemeralSecs, "Applies to new messages in this group, for everyone.");
                if (choice is not null) await groupsModel.SetTimerAsync(choice.Value);
            }));
            if (info.IsAdmin)
            {
                actions.Children.Add(Action(info.JoinPolicy == "public" ? "Make the group invite only" : "Let anyone with the link join", async () =>
                {
                    dialogOpen = false;
                    if (info.JoinPolicy != "public") { await groupsModel.SetJoinPolicyAsync(true); return; }
                    if (await ConfirmAsync("Make the group invite only?", info.HasLink == true
                            ? "Only invited people can join. The current invite link stops working at once."
                            : "Only invited people can join.", "Make invite only")) await groupsModel.SetJoinPolicyAsync(false);
                }));
                actions.Children.Add(Action(info.AnnouncementOnly ? "Let everyone post" : "Only admins can post", () => groupsModel.SetPostPolicyAsync(!info.AnnouncementOnly)));
                actions.Children.Add(Action(info.MembersCanInvite == true ? "Only admins can add people" : "Let members add people", () => groupsModel.SetMembersCanInviteAsync(info.MembersCanInvite != true)));
                actions.Children.Add(Action(info.MembersCanChangeTimer == true ? "Only admins can change the timer" : "Let members change the timer",
                    () => groupsModel.SetMembersCanChangeTimerAsync(info.MembersCanChangeTimer != true)));
                actions.Children.Add(Action("Edit name and description…", async () => { dialogOpen = false; await RenameGroupAsync(info); }));
                if (groupsModel.CanChooseWhoGoesLive)
                    actions.Children.Add(Action(info.EveryoneCanGoLive ? "Only admins can go live" : "Let everyone go live", () => groupsModel.SetStreamPolicyAsync(!info.EveryoneCanGoLive)));
            }
            if (info.IsOwner)
            {
                var candidates = GroupAdmin.TransferCandidates(info, members);
                if (candidates.Count > 0) actions.Children.Add(Action("Transfer ownership…", async () => { dialogOpen = false; await TransferGroupAsync(candidates); }));
                else if (!GroupAdmin.MayLeave(info))
                    actions.Children.Add(new TextBlock { Text = "To leave, first make another member an admin and transfer ownership to them.", TextWrapping = TextWrapping.Wrap, FontSize = 12, Opacity = 0.8 });
            }
            if (GroupAdmin.MayLeave(info)) actions.Children.Add(Action("Leave group", async () =>
            {
                dialogOpen = false;
                if (await ConfirmAsync($"Leave {info.DisplayName}?", "You stop receiving its messages. Messages already on this device stay.", "Leave")) await groupsModel.LeaveAsync();
            }));
            if (info.IsOwner) actions.Children.Add(Action("Delete group", async () =>
            {
                dialogOpen = false;
                if (await ConfirmAsync($"Delete {info.DisplayName}?", "Every member loses access. The postbox deletes its content within 30 days.", "Delete group")) await groupsModel.DeleteAsync();
            }));
            panel.Children.Add(actions);

            dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = info.DisplayName,
                Content = new ScrollViewer { Content = panel, MaxHeight = 620 }, CloseButtonText = "Close",
            };
            FrameworkElement MemberRow(GroupMemberRow member)
            {
                var row = new Grid { ColumnSpacing = 10 };
                row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
                row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
                row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
                var face = new Border { Width = 32, Height = 32, CornerRadius = new CornerRadius(16), Tag = groupsModel.KeyOf(member.Address), Background = new SolidColorBrush(Microsoft.UI.ColorHelper.FromArgb(0x33, 0x80, 0x88, 0x99)) };
                face.Child = new TextBlock { Text = member.Initials, FontSize = 11, HorizontalAlignment = HorizontalAlignment.Center, VerticalAlignment = VerticalAlignment.Center, IsHitTestVisible = false };
                face.Loaded += Avatar_Loaded;
                face.Unloaded += Avatar_Unloaded;
                row.Children.Add(face);
                var text = new StackPanel();
                var badge = member.State == "banned" ? "Banned" : member.RoleLabel;
                text.Children.Add(new TextBlock { Text = member.Name + (member.IsMe ? " (you)" : "") + (badge.Length > 0 ? " · " + badge : ""), FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, TextTrimming = TextTrimming.CharacterEllipsis });
                text.Children.Add(new TextBlock { Text = groupsModel.KeyOf(member.Address), FontSize = 11.5, Opacity = 0.75, IsTextSelectionEnabled = true });
                if (member.State != "banned")
                {
                    var devices = new TextBlock { Text = member.DevicesLabel, FontSize = 11.5, FontFamily = (FontFamily)Application.Current.Resources["MonoFont"], IsTextSelectionEnabled = true, TextWrapping = TextWrapping.Wrap };
                    AutomationProperties.SetName(devices, $"{member.Name} devices: {member.DevicesLabel}");
                    text.Children.Add(devices);
                }
                Grid.SetColumn(text, 1);
                row.Children.Add(text);
                var menu = new MenuFlyout();
                foreach (var action in GroupAdmin.ActionsFor(info, member))
                    Item(menu, GroupAdmin.Label(action), () => MemberActionAsync(member, action));
                if (!member.IsMe)
                {
                    var key = groupsModel.KeyOf(member.Address);
                    Item(menu, "Message", () => OpenPeerAsync(key));
                    Item(menu, "Block…", async () =>
                    {
                        dialogOpen = false;
                        await ConfirmBlockAsync(key, member.Name, report: false);
                        groupsModel.SetContacts(ViewModel.Contacts);
                        await groupsModel.SelectAsync(groupsModel.Selected);
                    });
                }
                if (menu.Items.Count > 0)
                {
                    var more = new Button { Content = new FontIcon { Glyph = "\uE712", FontSize = 13 }, Style = (Style)Root.Resources["QuietIconButton"] };
                    AutomationProperties.SetName(more, member.IsMe ? "Your actions" : "Actions for " + member.Name);
                    more.Flyout = menu;
                    Grid.SetColumn(more, 2);
                    row.Children.Add(more);
                }
                return row;
            }
            Button Action(string label, Func<Task> run)
            {
                var button = new Button { Content = label, HorizontalAlignment = HorizontalAlignment.Stretch, HorizontalContentAlignment = HorizontalAlignment.Left };
                button.Click += (_, _) => { pending = run; dialog?.Hide(); };
                return button;
            }
            void Item(MenuFlyout menu, string label, Func<Task> run)
            {
                var item = new MenuFlyoutItem { Text = label };
                item.Click += (_, _) => { pending = run; dialog?.Hide(); };
                menu.Items.Add(item);
            }
            await dialog.ShowAsync();
            dialogOpen = false;
            if (pending is not null) await pending();
            UpdateGroupPage();
        }
        finally { dialogOpen = false; }
    }

    private async Task MemberActionAsync(GroupMemberRow member, GroupMemberAction action)
    {
        dialogOpen = false;
        if (action == GroupMemberAction.Ban && !await ConfirmAsync($"Remove and ban {member.Name}?",
                $"{member.Name} loses access at once and cannot join again, even with a link, until an admin unbans them. Messages they already saw stay on their devices.", "Ban"))
            return;
        if (action == GroupMemberAction.Remove && !await ConfirmAsync($"Remove {member.Name} from the group?",
                $"{member.Name} loses access at once. Messages they already saw stay on their devices.", "Remove"))
            return;
        if (action == GroupMemberAction.StepDown && !await ConfirmAsync("Step down as admin?",
                "You stay in the group as a member. Only an admin can make you an admin again.", "Step down"))
            return;
        await groupsModel.MemberActionAsync(member, action);
        UpdateGroupPage();
    }

    // The new invite link, shown once: the postbox keeps only its hash.
    private async Task ShowGroupLinkAsync(string url)
    {
        if (dialogOpen) return;
        dialogOpen = true;
        try
        {
            var box = new TextBox { Text = url, IsReadOnly = true, FontFamily = (FontFamily)Application.Current.Resources["MonoFont"], TextWrapping = TextWrapping.Wrap };
            AutomationProperties.SetName(box, "Invite link");
            var panel = new StackPanel { Spacing = 10, MinWidth = 380, MaxWidth = 520 };
            panel.Children.Add(box);
            panel.Children.Add(new TextBlock { Text = "Shown once. Anyone with this link can join while it is on. Reset it to stop the old link working.", TextWrapping = TextWrapping.Wrap, FontSize = 12, Opacity = 0.8 });
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = "Invite link",
                Content = panel, PrimaryButtonText = "Copy link", CloseButtonText = "Done",
            };
            if (await dialog.ShowAsync() == ContentDialogResult.Primary) CopyText(url);
        }
        finally { dialogOpen = false; }
    }

    private async Task TransferGroupAsync(IReadOnlyList<GroupMemberRow> candidates)
    {
        if (dialogOpen || groupsModel.Info is not { } info) return;
        dialogOpen = true;
        GroupMemberRow? chosen = null;
        try
        {
            var pick = new ComboBox { Header = "New owner", MinWidth = 300 };
            foreach (var candidate in candidates) pick.Items.Add(new ComboBoxItem { Content = candidate.Name + " · " + groupsModel.KeyOf(candidate.Address), Tag = candidate });
            pick.SelectedIndex = 0;
            AutomationProperties.SetName(pick, "New owner");
            var panel = new StackPanel { Spacing = 10, MinWidth = 380 };
            panel.Children.Add(pick);
            panel.Children.Add(new TextBlock { Text = "Ownership goes to an admin. You stay an admin, and only the new owner can remove you.", TextWrapping = TextWrapping.Wrap, FontSize = 12, Opacity = 0.8 });
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = $"Transfer ownership of {info.DisplayName}",
                Content = panel, PrimaryButtonText = "Transfer", CloseButtonText = "Cancel",
            };
            if (await dialog.ShowAsync() != ContentDialogResult.Primary) return;
            chosen = (pick.SelectedItem as ComboBoxItem)?.Tag as GroupMemberRow;
        }
        finally { dialogOpen = false; }
        if (chosen is null) return;
        await groupsModel.TransferOwnershipAsync(chosen);
        UpdateGroupPage();
    }

    // Join by a /g/ link: paste it, see the group, then join (docs/groups.md §2, resolve then join).
    private async Task JoinByLinkAsync()
    {
        if (dialogOpen || !groupsModel.Enabled || ViewModel.SelectedMailbox is not { } mailbox) return;
        dialogOpen = true;
        (string GroupId, string? Token)? parsed = null;
        try
        {
            var link = new TextBox { Header = "Invite link", PlaceholderText = "https://inbox.pigeonpost.dev/g/…" };
            AutomationProperties.SetName(link, "Group invite link");
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = "Join with a link",
                Content = link, PrimaryButtonText = "Open link", CloseButtonText = "Cancel", IsPrimaryButtonEnabled = false,
            };
            link.TextChanged += (_, _) => dialog.IsPrimaryButtonEnabled = GroupAdmin.ParseInviteLink(link.Text) is not null;
            if (await dialog.ShowAsync() != ContentDialogResult.Primary) return;
            parsed = GroupAdmin.ParseInviteLink(link.Text);
        }
        finally { dialogOpen = false; }
        if (parsed is not { } target) return;
        if (sideView != SideView.Groups) ShowSideView(SideView.Groups);
        var preview = await groupsModel.PreviewLinkAsync(target.GroupId, target.Token);
        if (preview is null) { UpdateGroupPage(); return; }
        if (preview.MyState is "active" or "keying")
        {
            await groupsModel.SelectAsync(groupsModel.Groups.FirstOrDefault(g => g.Id == preview.Id));
            UpdateGroupPage();
            return;
        }
        var body = GroupAdmin.PreviewText(preview, mailbox.Key);
        if (preview.CanJoin != true) { await ShowNoticeAsync(preview.DisplayName, body); return; }
        if (!await ConfirmAsync($"Join {preview.DisplayName}?", body, "Join")) return;
        AccountStatus.Text = "Joining the group…";
        var joined = await groupsModel.JoinByLinkAsync(target.GroupId, target.Token);
        AccountStatus.Text = joined ? "Joined " + preview.DisplayName + "." : groupsModel.Error ?? "Could not join the group.";
        compactConversation = joined;
        UpdateLayoutMode();
        UpdateGroupPage();
    }

    private async Task ShowNoticeAsync(string title, string text)
    {
        if (dialogOpen) return;
        dialogOpen = true;
        try
        {
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = title,
                Content = new TextBlock { Text = text, TextWrapping = TextWrapping.Wrap }, CloseButtonText = "OK",
            };
            await dialog.ShowAsync();
        }
        finally { dialogOpen = false; }
    }

    private async Task InviteToGroupAsync()
    {
        if (dialogOpen) return;
        dialogOpen = true;
        string[] addresses = [];
        try
        {
            var who = new TextBox { Header = "Addresses", PlaceholderText = "/jane, /acme/ops" };
            AutomationProperties.SetName(who, "Addresses to invite");
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = "Add people",
                Content = who, PrimaryButtonText = "Invite", CloseButtonText = "Cancel",
            };
            if (await dialog.ShowAsync() != ContentDialogResult.Primary) return;
            addresses = Addresses(who.Text);
        }
        finally { dialogOpen = false; }
        if (addresses.Length == 0) return;
        var results = await groupsModel.InviteAsync(addresses);
        var skipped = results.Where(r => r.Status != "invited").Select(r => $"{r.Address} ({r.Status.Replace('_', ' ')})").ToArray();
        groupsModel.Status = skipped.Length == 0 ? "Invited." : "Invited the others. Not invited: " + string.Join(", ", skipped);
        UpdateGroupPage();
    }

    private async Task RenameGroupAsync(GroupInfo info)
    {
        if (dialogOpen) return;
        dialogOpen = true;
        string? name = null, description = null;
        try
        {
            var nameBox = new TextBox { Header = "Group name", Text = info.DisplayName, MaxLength = 80 };
            var descriptionBox = new TextBox { Header = "Description", Text = info.Description ?? "", MaxLength = 500, AcceptsReturn = true, TextWrapping = TextWrapping.Wrap };
            var panel = new StackPanel { Spacing = 12 };
            panel.Children.Add(nameBox);
            panel.Children.Add(descriptionBox);
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = "Edit group",
                Content = panel, PrimaryButtonText = "Save", CloseButtonText = "Cancel",
            };
            nameBox.TextChanged += (_, _) => dialog.IsPrimaryButtonEnabled = nameBox.Text.Trim().Length is >= 1 and <= 80;
            if (await dialog.ShowAsync() != ContentDialogResult.Primary) return;
            (name, description) = (nameBox.Text, descriptionBox.Text);
        }
        finally { dialogOpen = false; }
        await groupsModel.RenameAsync(name!, description);
    }

    // ---- group calls (WebView2 call host, the web client's own group calls) ------------------

    private async Task GroupCallAsync(bool video, bool join)
    {
        if (scheduledCall is not null) { AccountStatus.Text = "Finish joining the meeting first."; return; }
        if (groupsModel.Info is not { IsActive: true } info) return;
        if (CaptureCallPage() is not { } page || groupsModel.Mailbox != page.Actor.Mailbox.Address) { AccountStatus.Text = "Calling isn't ready yet. Try again in a moment."; return; }
        DesktopLog.Write($"{(join ? "Joining" : "Starting")} a group {(video ? "video" : "voice")} call");
        await RunCallScriptAsync(page, CallBridge.GroupCallScript(info.Id, info.DisplayName, video, join));
    }

    private int callStatusVersion;
    private (string? Group, DateTimeOffset At) lastCallCheck;
    private async Task UpdateGroupCallAsync()
    {
        if (groupJoinCall is null) return;
        var gid = groupsModel.Selected?.Id;
        if (lastCallCheck.Group == gid && DateTimeOffset.UtcNow - lastCallCheck.At < TimeSpan.FromSeconds(15)) return;
        lastCallCheck = (gid, DateTimeOffset.UtcNow);
        var version = ++callStatusVersion;
        var status = groupsModel.CanCall ? await groupsModel.CallStatusAsync(callPage) : null;
        if (version != callStatusVersion) return;
        groupJoinCall.Visibility = ToVisibility(status is { Active: true, Full: not true } && callMode == CallMode.Hidden);
    }
}

public sealed class GroupLineSelector : DataTemplateSelector
{
    public DataTemplate? Incoming { get; set; }
    public DataTemplate? Outgoing { get; set; }
    public DataTemplate? Notice { get; set; }
    protected override DataTemplate SelectTemplateCore(object item) => item switch
    {
        GroupLineRow { IsSystem: true } => Notice!,
        GroupLineRow { Mine: true } => Outgoing!,
        _ => Incoming!
    };
    protected override DataTemplate SelectTemplateCore(object item, DependencyObject container) => SelectTemplateCore(item);
}
