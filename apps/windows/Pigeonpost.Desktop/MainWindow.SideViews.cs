using System.Runtime.InteropServices.WindowsRuntime;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Microsoft.UI.Xaml.Media.Imaging;
using Pigeonpost.Core;
using Pigeonpost.Core.Groups;

namespace Pigeonpost.Desktop;

//  Phase 1b on Windows (SDS §12, §13, §15, §16, web inbox as the reference): the Calls page with its
//  missed badge, the Filtered list, Settings → Privacy and filtering and → Blocked, block actions
//  with their confirmation, profile pictures, and the disappearing-message timer of a conversation.
//  Every feature appears only when GET /v1/capabilities lists it for the account.
public sealed partial class MainWindow
{
    private enum SideView { Chats, Calls, Filtered, Groups }

    private ServerFeatures features = ServerFeatures.None;
    private SideView sideView = SideView.Chats;
    private CallHistoryModel callHistory = null!;
    private AvatarDirectory avatars = null!;
    private GroupsModel groupsModel = null!;
    private readonly System.Collections.ObjectModel.ObservableCollection<CallRow> callRows = [];
    private readonly System.Collections.ObjectModel.ObservableCollection<CallRow> filteredCallRows = [];
    private readonly System.Collections.ObjectModel.ObservableCollection<FilteredSender> filteredSenders = [];
    private IReadOnlyList<InboxMessage> filteredMessages = [];
    private Button? callsNav, groupsNav, chatsNav;
    private int featureVersion;
    private sealed record SideBinding(InboxViewModel Inbox, AuthorizationLease? Authorization, string Mailbox);
    private SideBinding? sideBinding;
    private SideBinding? refreshingSide;
    private bool SameAccount(InboxViewModel inbox, AuthorizationLease? authorization) => !lifetime.IsCancellationRequested
        && ReferenceEquals(ViewModel, inbox) && ReferenceEquals(session.AcceptedAuthorization, authorization) && authorization is not { IsActive: false };
    private bool SideCurrent(SideBinding binding) => ReferenceEquals(sideBinding, binding)
        && SameAccount(binding.Inbox, binding.Authorization) && ViewModel.SelectedMailbox?.Address == binding.Mailbox;

    private void CreateSideModels()
    {
        callHistory = new CallHistoryModel(postbox);
        avatars = new AvatarDirectory(postbox);
#if UI_TESTS
        var stores = new Dictionary<string, IGroupStateStore>(StringComparer.Ordinal);
        groupsModel = new GroupsModel(postbox, mailbox => stores.TryGetValue(mailbox, out var store) ? store : stores[mailbox] = new MemoryGroupStateStore());
#else
        var protector = new DpapiProtector();
        groupsModel = new GroupsModel(postbox, _ => new FileGroupStateStore(FileGroupStateStore.DefaultFolder(), protector));
#endif
        callHistory.EntriesChanged += (_, _) => { RebuildCallRows(); ViewModel.SetCallEntries(callHistory.Entries.ToArray()); };
        callHistory.PropertyChanged += (_, e) => { if (e.PropertyName is nameof(CallHistoryModel.UnseenMissed)) UpdateSideNav(); };
        avatars.PictureChanged += (_, key) => RefreshAvatarBorders(key);
        groupsModel.PropertyChanged += (_, e) => { if (e.PropertyName is nameof(GroupsModel.UnreadTotal) or nameof(GroupsModel.Invitations) or nameof(GroupsModel.Enabled)) UpdateSideNav(); };
        groupsModel.LinesChanged += (_, _) => ScrollGroupToEnd();
    }

    // Once the inbox is open, and again for every mailbox switch.
    private async Task StartSideViewsAsync()
    {
        var version = ++featureVersion;
        var inbox = ViewModel;
        var authorization = session.AcceptedAuthorization;
        ServerFeatures available;
        try
        {
            using var operation = authorization is null ? null : new AuthorizationContext(authorization);
            available = await postbox.GetCapabilitiesAsync(lifetime.Token);
        }
        catch (OperationCanceledException) { return; }
        catch (Exception ex) { DesktopLog.Error("Capabilities unavailable", ex); available = ServerFeatures.None; }
        if (version != featureVersion || !SameAccount(inbox, authorization)) return;
        features = available;
        DesktopLog.Write("Features: " + string.Join(",", features.Names.Order(StringComparer.Ordinal)));
        avatars.Enabled = features.Has(ServerFeatures.Avatars);
        ProfileMenuItem.Visibility = ToVisibility(features.Has(ServerFeatures.Avatars));
        PrivacyMenuItem.Visibility = ToVisibility(PrivacySettings.For(features).Count > 0);
        TimerMenuItem.Visibility = ToVisibility(features.Has(ServerFeatures.Disappearing));
        BuildSideNav();
        await MailboxChangedForSideViewsAsync();
    }

    private async Task MailboxChangedForSideViewsAsync()
    {
        sideBinding = null;
        refreshingSide = null;
        await groupsModel.ActivateAsync(null, ServerFeatures.None, [], []);
        avatars.Clear();
        AvatarFiles.Clear();
        if (ViewModel.SelectedMailbox is not { } mailbox) { callHistory.Reset(null); return; }
        var binding = new SideBinding(ViewModel, session.AcceptedAuthorization, mailbox.Address);
        sideBinding = binding;
        callHistory.Reset(features.Has(ServerFeatures.CallHistory) ? mailbox.Address : null);
        filteredMessages = [];
        filteredSenders.Clear();
        if (sideView != SideView.Chats) ShowSideView(SideView.Chats);
        UpdateFilteredEntry();
        await RefreshSideViewsAsync();
        if (!SideCurrent(binding)) return;
        await groupsModel.ActivateAsync(mailbox, features, binding.Inbox.Mailboxes, binding.Inbox.Contacts);
        if (!SideCurrent(binding)) return;
        RefreshAvatars();
        UpdateSideNav();
    }

    // Each refresh tick: call history (badge and missed lines), the Filtered list, and the groups loop.
    private async Task RefreshSideViewsAsync()
    {
        if (sideBinding is not { } binding || !SideCurrent(binding) || ReferenceEquals(refreshingSide, binding)) return;
        refreshingSide = binding;
        try
        {
            using var operation = binding.Authorization is null ? null : new AuthorizationContext(binding.Authorization);
            if (features.Has(ServerFeatures.CallHistory)) await callHistory.LoadAsync();
            if (!SideCurrent(binding)) return;
            if (features.Has(ServerFeatures.MessageFiltering))
            {
                try
                {
                    var messages = await postbox.GetFilteredAsync(binding.Mailbox, lifetime.Token);
                    if (SideCurrent(binding)) { filteredMessages = messages; RebuildFiltered(); }
                }
                catch (Exception ex) when (ex is not OperationCanceledException) { DesktopLog.Error("Filtered list unavailable", ex); }
            }
            if (!SideCurrent(binding)) return;
            if (groupsModel.Enabled) await groupsModel.RefreshAsync();
            if (!SideCurrent(binding)) return;
            if (sideView == SideView.Calls) await callHistory.MarkSeenAsync();
            if (SideCurrent(binding)) binding.Inbox.PruneExpired();
        }
        catch (OperationCanceledException) { }
        finally { if (ReferenceEquals(refreshingSide, binding)) refreshingSide = null; }
    }

    private void ResetSideViews()
    {
        featureVersion++;
        sideBinding = null;
        refreshingSide = null;
        features = ServerFeatures.None;
        callHistory.Reset(null);
        _ = groupsModel.ActivateAsync(null, ServerFeatures.None, [], []);
        avatars.Clear();
        avatars.Enabled = false;
        filteredMessages = [];
        filteredSenders.Clear();
        ShowSideView(SideView.Chats);
        SideNav.Visibility = NewGroupButton.Visibility = FilteredEntry.Visibility = Visibility.Collapsed;
        ProfileMenuItem.Visibility = PrivacyMenuItem.Visibility = TimerMenuItem.Visibility = Visibility.Collapsed;
    }

    private void SideViewsInboxUpdated()
    {
        groupsModel.SetContacts(ViewModel.Contacts);
        RebuildFiltered();
        RefreshAvatars();
    }

    // ---- the sidebar navigation ------------------------------------------------------------

    private void BuildSideNav()
    {
        SideNav.Children.Clear();
        Button Nav(string label, string automation, SideView view)
        {
            var button = new Button { Content = label, Padding = new Thickness(12, 4, 12, 5), MinHeight = 30, CornerRadius = new CornerRadius(8), FontSize = 13 };
            AutomationProperties.SetName(button, automation);
            button.Click += (_, _) => ShowSideView(view);
            SideNav.Children.Add(button);
            return button;
        }
        chatsNav = Nav("Chats", "Chats", SideView.Chats);
        callsNav = features.Has(ServerFeatures.CallHistory) ? Nav("Calls", "Calls", SideView.Calls) : null;
        groupsNav = features.Has(ServerFeatures.Groups) && GroupsModel.LibraryAvailable ? Nav("Groups", "Groups", SideView.Groups) : null;
        SideNav.Visibility = ToVisibility(callsNav is not null || groupsNav is not null);
        NewGroupButton.Visibility = ToVisibility(groupsNav is not null);
        UpdateSideNav();
    }

    private void UpdateSideNav()
    {
        if (callsNav is not null)
        {
            callsNav.Content = callHistory.HasBadge ? $"Calls  {callHistory.BadgeLabel}" : "Calls";
            AutomationProperties.SetName(callsNav, callHistory.AccessibleName);
        }
        if (groupsNav is not null)
        {
            var count = groupsModel.UnreadTotal + groupsModel.Invitations;
            groupsNav.Content = count > 0 ? $"Groups  {(count > 99 ? "99+" : count.ToString())}" : "Groups";
            AutomationProperties.SetName(groupsNav, count > 0 ? $"Groups, {count} new" : "Groups");
            NewGroupButton.Visibility = ToVisibility(groupsModel.Enabled);
        }
        foreach (var (button, view) in new[] { (chatsNav, SideView.Chats), (callsNav, SideView.Calls), (groupsNav, SideView.Groups) })
            if (button is not null) button.Style = (Style)Application.Current.Resources[sideView == view ? "AccentButtonStyle" : "DefaultButtonStyle"];
    }

    private void ShowSideView(SideView view)
    {
        sideView = view;
        SidePage.Children.Clear();
        SideListHost.Children.Clear();
        switch (view)
        {
            case SideView.Calls: SidePage.Children.Add(BuildCallsPage()); _ = callHistory.MarkSeenAsync(); break;
            case SideView.Filtered: SidePage.Children.Add(BuildFilteredPage()); break;
            case SideView.Groups: SideListHost.Children.Add(BuildGroupList()); SidePage.Children.Add(BuildGroupPage()); break;
        }
        UpdateLayoutMode();
        UpdateSideNav();
        UpdateFilteredEntry();
    }

    // Called at the end of UpdateLayoutMode: a side page takes the subject and message columns.
    private void ApplySideLayout(bool compact)
    {
        if (SidePage is null) return;
        var page = sideView != SideView.Chats;
        SidePage.Visibility = ToVisibility(page && (!compact || compactConversation));
        SideListHost.Visibility = ToVisibility(sideView == SideView.Groups && (!compact || !compactConversation));
        ConversationListHost.Visibility = ToVisibility(sideView != SideView.Groups);
        if (!page) return;
        MessagingArea.Visibility = SubjectPane.Visibility = SubjectDivider.Visibility = Visibility.Collapsed;
        if (compact)
        {
            SenderPane.Visibility = ToVisibility(!compactConversation);
            SenderColumn.Width = compactConversation ? new GridLength(0) : new GridLength(1, GridUnitType.Star);
            MessageColumn.Width = compactConversation ? new GridLength(1, GridUnitType.Star) : new GridLength(0);
        }
        else MessageColumn.Width = new GridLength(1, GridUnitType.Star);
    }

    private Grid PageFrame(string title, out StackPanel actions, out Grid body)
    {
        var frame = new Grid { RowSpacing = 0 };
        frame.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });
        frame.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) });
        var header = new Grid { Padding = new Thickness(24, 14, 20, 14), ColumnSpacing = 10 };
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
        var back = new Button { Content = new SymbolIcon(Symbol.Back), Style = (Style)Root.Resources["QuietIconButton"], Visibility = ToVisibility(IsCompact) };
        AutomationProperties.SetName(back, "Back");
        back.Click += (_, _) => { compactConversation = false; UpdateLayoutMode(); };
        header.Children.Add(back);
        var heading = new TextBlock { Text = title, FontSize = 18, FontWeight = Microsoft.UI.Text.FontWeights.SemiBold, VerticalAlignment = VerticalAlignment.Center };
        AutomationProperties.SetHeadingLevel(heading, Microsoft.UI.Xaml.Automation.Peers.AutomationHeadingLevel.Level1);
        Grid.SetColumn(heading, 1);
        header.Children.Add(heading);
        actions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        Grid.SetColumn(actions, 2);
        header.Children.Add(actions);
        frame.Children.Add(header);
        body = new Grid();
        Grid.SetRow(body, 1);
        frame.Children.Add(body);
        return frame;
    }

    // ---- Calls page --------------------------------------------------------------------------

    private void RebuildCallRows()
    {
        var contacts = ViewModel.Contacts;
        StableList.Reconcile(callRows, callHistory.Entries.Select(e => Pigeonpost.Core.CallRow.For(e, contacts)).ToArray(), r => r.Entry.Id, (a, b) => a == b);
        StableList.Reconcile(filteredCallRows, callHistory.FilteredEntries.Select(e => Pigeonpost.Core.CallRow.For(e, contacts)).ToArray(), r => r.Entry.Id, (a, b) => a == b);
        if (callsEmpty is not null) { callsEmpty.Text = callHistory.Error ?? callHistory.EmptyText; callsEmpty.Visibility = ToVisibility(callsEmpty.Text.Length > 0); }
        if (callsMore is not null) callsMore.Visibility = ToVisibility(callHistory.CanLoadMore);
        if (filteredCallsExpander is not null) filteredCallsExpander.Header = callHistory.FilteredTitle;
    }

    private TextBlock? callsEmpty;
    private Button? callsMore;
    private Expander? filteredCallsExpander;

    private FrameworkElement BuildCallsPage()
    {
        var frame = PageFrame("Calls", out var actions, out var body);
        var filter = new ComboBox { MinWidth = 120, Style = (Style)Root.Resources["SidebarComboBox"] };
        filter.Items.Add(new ComboBoxItem { Content = "All" });
        filter.Items.Add(new ComboBoxItem { Content = "Missed" });
        filter.SelectedIndex = callHistory.Filter == "missed" ? 1 : 0;
        AutomationProperties.SetName(filter, "Call filter");
        filter.SelectionChanged += (_, _) => callHistory.Filter = filter.SelectedIndex == 1 ? "missed" : "all";
        actions.Children.Add(filter);
        var clear = new Button { Content = "Clear all", Style = (Style)Root.Resources["SecondaryActionButton"] };
        AutomationProperties.SetName(clear, "Clear all calls");
        clear.Click += async (_, _) =>
        {
            if (sideBinding is not { } binding || !SideCurrent(binding)) return;
            if (!await ConfirmAsync("Clear your call history?", "Every call in this mailbox's history is deleted. This can't be undone.", "Clear all")) return;
            await RunCallHistoryActionAsync(binding, callHistory.ClearAsync);
        };
        actions.Children.Add(clear);
        var stack = new StackPanel { Padding = new Thickness(20, 8, 20, 20), Spacing = 4 };
        var list = new ListView
        {
            ItemsSource = callRows, ItemTemplate = (DataTemplate)Root.Resources["CallRowTemplate"], SelectionMode = ListViewSelectionMode.None,
        };
        ScrollViewer.SetVerticalScrollMode(list, ScrollMode.Disabled);
        AutomationProperties.SetName(list, "Call history");
        stack.Children.Add(list);
        callsEmpty = new TextBlock { Margin = new Thickness(8, 16, 8, 8), FontSize = 13, TextWrapping = TextWrapping.Wrap };
        stack.Children.Add(callsEmpty);
        callsMore = new Button { Content = "Show older calls", HorizontalAlignment = HorizontalAlignment.Center };
        callsMore.Click += async (_, _) => await callHistory.LoadAsync(more: true);
        stack.Children.Add(callsMore);
        var filteredList = new ListView { ItemsSource = filteredCallRows, ItemTemplate = (DataTemplate)Root.Resources["CallRowTemplate"], SelectionMode = ListViewSelectionMode.None };
        AutomationProperties.SetName(filteredList, "Filtered calls");
        var filteredPanel = new StackPanel { Spacing = 6 };
        filteredPanel.Children.Add(new TextBlock { Text = "Calls silenced by your Privacy and filtering settings. They never rang.", FontSize = 12.5, TextWrapping = TextWrapping.Wrap });
        filteredPanel.Children.Add(filteredList);
        filteredCallsExpander = new Expander { Header = callHistory.FilteredTitle, Content = filteredPanel, IsExpanded = false, HorizontalAlignment = HorizontalAlignment.Stretch, HorizontalContentAlignment = HorizontalAlignment.Stretch, Margin = new Thickness(0, 16, 0, 0) };
        AutomationProperties.SetName(filteredCallsExpander, "Filtered calls section");
        stack.Children.Add(filteredCallsExpander);
        body.Children.Add(new ScrollViewer { Content = stack });
        RebuildCallRows();
        return frame;
    }

    private async void CallRow_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement { Tag: CallRow { Key: { } key } }) return;
        await OpenPeerAsync(key);
    }

    private async void CallBack_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement { Tag: CallRow { Key: { } key } row } || sideBinding is not { } binding || !SideCurrent(binding)
            || callSession is not { } actor || !CallCurrent(actor)) return;
        await OpenPeerAsync(key);
        if (SideCurrent(binding) && CallCurrent(actor)) await PlaceCallAsync(key, row.Entry.IsVideo);
    }

    private async void DeleteCall_Click(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: CallRow row } && sideBinding is { } binding) await RunCallHistoryActionAsync(binding, () => callHistory.DeleteAsync(row.Entry));
    }

    private async void ReportCall_Click(object sender, RoutedEventArgs e)
    {
        if (sender is FrameworkElement { Tag: CallRow row } && sideBinding is { } binding) await RunCallHistoryActionAsync(binding, () => callHistory.ReportAsync(row.Entry));
    }

    private async Task RunCallHistoryActionAsync(SideBinding binding, Func<Task<string>> action)
    {
        if (!SideCurrent(binding)) return;
        try
        {
            using var operation = binding.Authorization is null ? null : new AuthorizationContext(binding.Authorization);
            var result = await action();
            if (SideCurrent(binding)) AccountStatus.Text = result;
        }
        catch (OperationCanceledException) { }
    }

    private async void BlockCall_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement { Tag: CallRow { Key: { } key } row }) return;
        await ConfirmBlockAsync(key, row.Name, report: false, call: row.Entry.IsIncoming ? row.Entry : null);
    }

    // Open (or start) the 1:1 conversation with a peer from another page.
    private async Task OpenPeerAsync(string peer)
    {
        if (sideBinding is not { } binding || !SideCurrent(binding)) return;
        ShowSideView(SideView.Chats);
        try
        {
            using var operation = binding.Authorization is null ? null : new AuthorizationContext(binding.Authorization);
            var inbox = binding.Inbox;
            var existing = inbox.AllConversations.FirstOrDefault(c => c.Peer == PostAddress.Canonical(peer));
            if (existing is not null)
            {
                inbox.SenderSearch = "";
                inbox.ShowArchived = false;
                inbox.SelectConversation(inbox.Conversations.FirstOrDefault(c => c.Peer == existing.Peer) ?? existing);
            }
            else await inbox.StartConversationAsync(peer, "");
            if (SideCurrent(binding)) ShowConversation();
        }
        catch (OperationCanceledException) { }
    }

    // ---- Filtered ------------------------------------------------------------------------------

    private void RebuildFiltered()
    {
        var senders = features.Has(ServerFeatures.MessageFiltering) ? FilteredSender.Group(filteredMessages, ViewModel.Contacts) : [];
        StableList.Reconcile(filteredSenders, senders, s => s.Peer, (a, b) => a.Peer == b.Peer && a.Messages.Count == b.Messages.Count && a.Last == b.Last);
        UpdateFilteredEntry();
        if (filteredEmpty is not null) filteredEmpty.Visibility = ToVisibility(filteredSenders.Count == 0);
    }

    private void UpdateFilteredEntry()
    {
        var count = filteredSenders.Count;
        FilteredEntry.Visibility = ToVisibility(features.Has(ServerFeatures.MessageFiltering) && count > 0 && sideView == SideView.Chats && !ViewModel.ShowArchived);
        FilteredEntry.Content = $"Filtered ({count})";
        AutomationProperties.SetName(FilteredEntry, $"Filtered, {count} {(count == 1 ? "conversation" : "conversations")}");
    }

    private TextBlock? filteredEmpty;

    private FrameworkElement BuildFilteredPage()
    {
        var frame = PageFrame("Filtered", out _, out var body);
        var stack = new StackPanel { Padding = new Thickness(24, 12, 24, 24), Spacing = 6, MaxWidth = 760, HorizontalAlignment = HorizontalAlignment.Left };
        stack.Children.Add(new TextBlock
        {
            Text = "Messages your Privacy and filtering settings kept out of your inbox. They didn't notify you. Accept moves a sender's messages to your inbox and adds them to your contacts.",
            TextWrapping = TextWrapping.Wrap, FontSize = 13, Opacity = 0.8,
        });
        var list = new ListView { ItemsSource = filteredSenders, ItemTemplate = (DataTemplate)Root.Resources["FilteredRowTemplate"], SelectionMode = ListViewSelectionMode.None };
        ScrollViewer.SetVerticalScrollMode(list, ScrollMode.Disabled);
        AutomationProperties.SetName(list, "Filtered senders");
        stack.Children.Add(list);
        filteredEmpty = new TextBlock { Text = "Nothing is filtered.", Margin = new Thickness(0, 12, 0, 0), Visibility = ToVisibility(filteredSenders.Count == 0) };
        stack.Children.Add(filteredEmpty);
        body.Children.Add(new ScrollViewer { Content = stack });
        return frame;
    }

    private void FilteredEntry_Click(object sender, RoutedEventArgs e) { compactConversation = true; ShowSideView(SideView.Filtered); }

    private async void AcceptFiltered_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement { Tag: FilteredSender row } || ViewModel.SelectedMailbox is not { } mailbox) return;
        try
        {
            await postbox.AcceptFilteredAsync(mailbox.Address, row.Peer, lifetime.Token);
            filteredMessages = filteredMessages.Where(m => !row.Messages.Contains(m)).ToArray();
            RebuildFiltered();
            AccountStatus.Text = $"Accepted {row.Name}. Their messages are in your inbox.";
            await ViewModel.RefreshAsync();
        }
        catch (Exception ex) when (ex is not OperationCanceledException) { AccountStatus.Text = "Could not accept. " + (ex as PostboxException)?.Message; }
    }

    private async void BlockFiltered_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement { Tag: FilteredSender row }) return;
        // From Filtered, "Also report as spam" starts checked (SDS §15.2).
        if (await ConfirmBlockAsync(row.Peer, row.Name, report: true, messageId: row.Messages.LastOrDefault()?.MessageId))
        {
            filteredMessages = filteredMessages.Where(m => !row.Messages.Contains(m)).ToArray();
            RebuildFiltered();
        }
    }

    private async void ReportFiltered_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement { Tag: FilteredSender row } || ViewModel.SelectedMailbox is not { } mailbox || row.Messages.LastOrDefault() is not { } last) return;
        try { await postbox.ReportMessageAsync(mailbox.Address, last.MessageId, lifetime.Token); AccountStatus.Text = "Reported as spam."; }
        catch (Exception ex) when (ex is not OperationCanceledException) { AccountStatus.Text = "Could not send the report."; }
    }

    // ---- blocking ------------------------------------------------------------------------------

    // The confirmation sheet of SDS §15.2: one option, Also report as spam; a namespace-wide block
    // when the namespace is owned. Returns true once the block is saved.
    private async Task<bool> ConfirmBlockAsync(string peer, string name, bool report, string? messageId = null, CallLogEntry? call = null)
    {
        if (dialogOpen || ViewModel.SelectedMailbox is not { } mailbox) return false;
        dialogOpen = true;
        try
        {
            var panel = new StackPanel { Spacing = 12, MaxWidth = 440 };
            panel.Children.Add(new TextBlock { Text = BlockRules.Confirmation, TextWrapping = TextWrapping.Wrap });
            var alsoReport = new CheckBox { Content = "Also report as spam", IsChecked = report };
            panel.Children.Add(alsoReport);
            var wide = BlockRules.WildcardFor(peer);
            var everyone = new CheckBox { Content = wide is null ? "" : "Block everyone at " + wide[..^2], Visibility = ToVisibility(wide is not null) };
            panel.Children.Add(everyone);
            var error = new TextBlock { TextWrapping = TextWrapping.Wrap, Foreground = new SolidColorBrush(Microsoft.UI.Colors.IndianRed) };
            panel.Children.Add(error);
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = $"Block {name}?", Content = panel,
                PrimaryButtonText = "Block", CloseButtonText = "Cancel", DefaultButton = ContentDialogButton.Close,
            };
            var saved = false;
            dialog.PrimaryButtonClick += async (_, args) =>
            {
                var deferral = args.GetDeferral();
                try
                {
                    var target = everyone.IsChecked == true && wide is not null ? wide : peer;
                    if (call is not null)
                    {
                        saved = await ViewModel.BlockAsync(target, null, report: false);
                        if (saved && alsoReport.IsChecked == true) AccountStatus.Text = "Blocked. " + await callHistory.ReportAsync(call);
                    }
                    else saved = await ViewModel.BlockAsync(target, null, alsoReport.IsChecked == true, messageId);
                    if (!saved) { error.Text = ViewModel.Error ?? "Could not block. Please try again."; args.Cancel = true; }
                    else if (call is null) AccountStatus.Text = alsoReport.IsChecked == true ? "Blocked and reported as spam." : "Blocked. They won't be able to message or call you.";
                }
                finally { deferral.Complete(); }
            };
            await dialog.ShowAsync();
            if (saved) { RebuildFiltered(); groupsModel.SetContacts(ViewModel.Contacts); }
            return saved;
        }
        finally { dialogOpen = false; }
    }

    private async void BlockConversation_Click(object sender, RoutedEventArgs e)
    {
        if (ViewModel.SelectedConversation is not { } conversation) return;
        if (ViewModel.IsSelectedBlocked) { await UnblockSelectedAsync(); return; }
        await ConfirmBlockAsync(conversation.Peer, conversation.Name, report: false);
    }

    private async void BlockSender_Click(object sender, RoutedEventArgs e)
    {
        if (sender is not FrameworkElement { Tag: string messageId } || ViewModel.SelectedConversation is not { } conversation) return;
        await ConfirmBlockAsync(conversation.Peer, conversation.Name, report: false, messageId: messageId);
    }

    private async void Unblock_Click(object sender, RoutedEventArgs e) => await UnblockSelectedAsync();

    private async Task UnblockSelectedAsync()
    {
        if (ViewModel.SelectedConversation is not { } conversation) return;
        if (await ViewModel.UnblockAsync(conversation.Peer)) AccountStatus.Text = $"Unblocked {conversation.Name}.";
    }

    // Settings → Blocked: exact and wildcard blocks, Unblock, and blocking an address directly.
    private async void Blocked_Click(object sender, RoutedEventArgs e)
    {
        if (dialogOpen || ViewModel.SelectedMailbox is not { } mailbox) return;
        dialogOpen = true;
        try
        {
            var panel = new StackPanel { Spacing = 10, MinWidth = 380, MaxWidth = 480 };
            var list = new StackPanel { Spacing = 6 };
            var status = new TextBlock { TextWrapping = TextWrapping.Wrap, FontSize = 12.5 };
            var input = new TextBox { Header = "Block an address", PlaceholderText = "/k/…, /spamco/agent or /spamco/*", MaxLength = 512 };
            AutomationProperties.SetName(input, "Address to block");
            var add = new Button { Content = "Block" };
            AutomationProperties.SetName(add, "Block this address");
            async Task Reload()
            {
                list.Children.Clear();
                IReadOnlyList<Contact> rows;
                try { rows = await postbox.GetBlockedAsync(mailbox.Address, lifetime.Token); }
                catch (Exception ex) when (ex is not OperationCanceledException)
                {
                    rows = ViewModel.Contacts.Where(c => c.Admission == "block").ToArray();
                    status.Text = "Could not refresh the list. Showing what was last loaded.";
                }
                if (rows.Count == 0) list.Children.Add(new TextBlock { Text = "Nobody is blocked." });
                foreach (var rule in rows)
                {
                    var row = new Grid { ColumnSpacing = 10 };
                    row.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
                    row.ColumnDefinitions.Add(new ColumnDefinition { Width = GridLength.Auto });
                    var who = new StackPanel();
                    who.Children.Add(new TextBlock { Text = rule.Peer, FontFamily = (FontFamily)Application.Current.Resources["MonoFont"], TextTrimming = TextTrimming.CharacterEllipsis, IsTextSelectionEnabled = true });
                    who.Children.Add(new TextBlock { Text = BlockRules.Describe(rule), FontSize = 12 });
                    row.Children.Add(who);
                    var unblock = new Button { Content = "Unblock" };
                    AutomationProperties.SetName(unblock, "Unblock " + rule.Peer);
                    unblock.Click += async (_, _) =>
                    {
                        unblock.IsEnabled = false;
                        try
                        {
                            await postbox.UnblockAsync(mailbox.Address, rule.Peer, lifetime.Token);
                            status.Text = "Unblocked " + rule.Peer + ".";
                            await ViewModel.RefreshAsync();
                            await Reload();
                        }
                        catch (Exception ex) when (ex is not OperationCanceledException) { unblock.IsEnabled = true; status.Text = $"Could not unblock {rule.Peer}. {(ex as PostboxException)?.Message}"; }
                    };
                    Grid.SetColumn(unblock, 1);
                    row.Children.Add(unblock);
                    list.Children.Add(row);
                }
            }
            add.Click += async (_, _) =>
            {
                var peer = PostAddress.Input(input.Text);
                if (!BlockRules.IsBlockable(peer)) { status.Text = "Enter an address such as /k/…, /spamco/agent or /spamco/*."; return; }
                if (peer == mailbox.Address || PostAddress.Canonical(peer) == PostAddress.Canonical(mailbox.Key)) { status.Text = "You cannot block this mailbox itself."; return; }
                add.IsEnabled = false;
                try
                {
                    await postbox.BlockAsync(mailbox.Address, peer, null, lifetime.Token);
                    input.Text = "";
                    status.Text = "Blocked. They won't be able to message or call you.";
                    await ViewModel.RefreshAsync();
                    await Reload();
                }
                catch (Exception ex) when (ex is not OperationCanceledException) { status.Text = "Could not block. " + (ex as PostboxException)?.Message; }
                finally { add.IsEnabled = true; }
            };
            panel.Children.Add(new TextBlock { Text = "Blocked people can't message or call this mailbox, and aren't told.", TextWrapping = TextWrapping.Wrap });
            panel.Children.Add(list);
            panel.Children.Add(input);
            panel.Children.Add(add);
            panel.Children.Add(status);
            await Reload();
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = "Blocked",
                Content = new ScrollViewer { Content = panel, MaxHeight = 520 }, CloseButtonText = "Done",
            };
            await dialog.ShowAsync();
            RebuildFiltered();
        }
        finally { dialogOpen = false; }
    }

    // Settings → Privacy and filtering: the six controls of the web, each saved on change.
    private async void Privacy_Settings_Click(object sender, RoutedEventArgs e)
    {
        if (dialogOpen || ViewModel.SelectedMailbox is not { } mailbox) return;
        dialogOpen = true;
        try
        {
            var panel = new StackPanel { Spacing = 16, MinWidth = 380, MaxWidth = 500 };
            var status = new TextBlock { TextWrapping = TextWrapping.Wrap, FontSize = 12.5 };
            InboxPolicy? policy = null;
            try { policy = await postbox.GetPolicyAsync(mailbox.Address, lifetime.Token); }
            catch (Exception ex) when (ex is not OperationCanceledException) { status.Text = "Could not load these settings. Close and try again."; }
            panel.Children.Add(new TextBlock { Text = "For " + mailbox.Key, FontFamily = (FontFamily)Application.Current.Resources["MonoFont"], FontSize = 12.5 });
            foreach (var setting in PrivacySettings.For(features))
            {
                var box = new ComboBox { Header = setting.Title, MinWidth = 220, IsEnabled = policy is not null };
                AutomationProperties.SetName(box, setting.Title);
                foreach (var option in setting.Options) box.Items.Add(new ComboBoxItem { Content = option.Label, Tag = option.Value });
                var current = policy?.Value(setting.Field);
                box.SelectedItem = box.Items.Cast<ComboBoxItem>().FirstOrDefault(i => (string)i.Tag == current);
                var previous = box.SelectedItem;
                box.SelectionChanged += async (_, _) =>
                {
                    if (box.SelectedItem is not ComboBoxItem { Tag: string value } item || ReferenceEquals(item, previous)) return;
                    try
                    {
                        await postbox.SetPolicyAsync(mailbox.Address, setting.Field, PrivacySettings.Encode(setting.Field, value), lifetime.Token);
                        previous = item;
                        status.Text = "Saved.";
                    }
                    catch (Exception ex) when (ex is not OperationCanceledException)
                    {
                        box.SelectedItem = previous;
                        status.Text = "Could not save that setting. Please try again.";
                    }
                };
                var row = new StackPanel { Spacing = 4 };
                row.Children.Add(box);
                if (setting.Note is { } note) row.Children.Add(new TextBlock { Text = note, FontSize = 12, TextWrapping = TextWrapping.Wrap, Opacity = 0.75 });
                panel.Children.Add(row);
            }
            panel.Children.Add(status);
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = "Privacy and filtering",
                Content = new ScrollViewer { Content = panel, MaxHeight = 560 }, CloseButtonText = "Done",
            };
            await dialog.ShowAsync();
        }
        finally { dialogOpen = false; }
        await RefreshSideViewsAsync();
    }

    // Disappearing messages for the selected subject (both sides of the thread share it).
    private async void Timer_Click(object sender, RoutedEventArgs e)
    {
        if (dialogOpen || !ViewModel.CanSetTimer) return;
        var choice = await ChooseTimerAsync(ViewModel.SelectedSubject?.EphemeralSecs, "Applies to new messages in this subject, for both of you.");
        if (choice is not null && await ViewModel.SetTimerAsync(choice.Value))
            AccountStatus.Text = choice.Value is null ? "Disappearing messages are off." : Timers.Banner(choice.Value) + ".";
    }

    private async Task<TimerChoice?> ChooseTimerAsync(int? current, string note)
    {
        if (dialogOpen) return null;
        dialogOpen = true;
        try
        {
            var options = new RadioButtons { Header = "Messages disappear after" };
            AutomationProperties.SetName(options, "Disappearing messages");
            foreach (var choice in Timers.Choices) options.Items.Add(new RadioButton { Content = Timers.Label(choice), Tag = choice });
            options.SelectedIndex = Math.Max(0, Timers.Choices.ToList().IndexOf(current is > 0 ? current : null));
            var panel = new StackPanel { Spacing = 12, MaxWidth = 440 };
            panel.Children.Add(options);
            panel.Children.Add(new TextBlock { Text = note + " The clock starts when a message is sent.", TextWrapping = TextWrapping.Wrap, FontSize = 12.5 });
            panel.Children.Add(new TextBlock { Text = Timers.Limits, TextWrapping = TextWrapping.Wrap, FontSize = 12, Opacity = 0.75 });
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = "Disappearing messages",
                Content = panel, PrimaryButtonText = "Save", CloseButtonText = "Cancel",
            };
            if (await dialog.ShowAsync() != ContentDialogResult.Primary || options.SelectedItem is not RadioButton { Tag: var tag }) return null;
            return new TimerChoice(tag as int?);
        }
        finally { dialogOpen = false; }
    }

    private sealed record TimerChoice(int? Value);

    private async Task<bool> ConfirmAsync(string title, string text, string action)
    {
        if (dialogOpen) return false;
        dialogOpen = true;
        try
        {
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = title,
                Content = new TextBlock { Text = text, TextWrapping = TextWrapping.Wrap }, PrimaryButtonText = action, CloseButtonText = "Cancel",
            };
            return await dialog.ShowAsync() == ContentDialogResult.Primary;
        }
        finally { dialogOpen = false; }
    }

    // ---- profile pictures ----------------------------------------------------------------------

    private readonly Dictionary<string, List<Border>> avatarBorders = new(StringComparer.Ordinal);

    private void Avatar_Loaded(object sender, RoutedEventArgs e)
    {
        if (sender is not Border { Tag: string key } border || key.Length == 0) return;
        key = AvatarDirectory.Key(key);
        if (!avatarBorders.TryGetValue(key, out var list)) avatarBorders[key] = list = [];
        if (!list.Contains(border)) list.Add(border);
        _ = PaintAvatarAsync(border, key);
    }

    private void Avatar_Unloaded(object sender, RoutedEventArgs e)
    {
        if (sender is Border { Tag: string key } border && avatarBorders.TryGetValue(AvatarDirectory.Key(key), out var list)) list.Remove(border);
    }

    private void RefreshAvatarBorders(string key)
    {
        if (!avatarBorders.TryGetValue(key, out var list)) return;
        foreach (var border in list.ToArray()) _ = PaintAvatarAsync(border, key);
    }

    private async Task PaintAvatarAsync(Border border, string key)
    {
        var version = avatars.Version;
        bool Current() => !lifetime.IsCancellationRequested && avatars.IsCurrent(version)
            && border.Tag is string tag && AvatarDirectory.Key(tag) == key;
        try
        {
            var bytes = await avatars.ImageAsync(key, lifetime.Token);
            if (!Current()) return;
            if (bytes is null)
            {
                border.ClearValue(Border.BackgroundProperty);
                if (border.Child is UIElement fallbackInitials) fallbackInitials.Opacity = 1;
                return;
            }
            var image = new BitmapImage { DecodePixelWidth = 96 };
            using var stream = new Windows.Storage.Streams.InMemoryRandomAccessStream();
            await stream.WriteAsync(bytes.AsBuffer());
            stream.Seek(0);
            await image.SetSourceAsync(stream);
            if (!Current() || !avatars.TryWithCurrent(version, () => AvatarFiles.Remember(key, bytes))) return;
            border.Background = new ImageBrush { ImageSource = image, Stretch = Stretch.UniformToFill };
            if (border.Child is UIElement initials) initials.Opacity = 0;
        }
        catch (OperationCanceledException) { }
        catch (Exception ex) { DesktopLog.Error("Showing a picture failed", ex); }
    }

    private void RefreshAvatars()
    {
        if (!avatars.Enabled || sideBinding is not { } binding || !SideCurrent(binding)) return;
        var keys = ViewModel.AllConversations.Select(c => c.Peer)
            .Concat(callHistory.Entries.Select(e => e.PeerKey).OfType<string>())
            .Concat(filteredSenders.Select(s => s.Peer))
            .Concat(ViewModel.Mailboxes.Select(m => m.Key)).Take(300).ToArray();
        _ = avatars.RefreshAsync(keys, lifetime.Token);
    }

    // Settings → Profile picture: pick a file, square crop with zoom, who can see it, all mailboxes.
    private async void ProfilePicture_Click(object sender, RoutedEventArgs e)
    {
        if (dialogOpen || ViewModel.SelectedMailbox is not { } mailbox) return;
        dialogOpen = true;
        try
        {
            AvatarInfo? own = null;
            try { own = await postbox.GetOwnAvatarAsync(mailbox.Address, lifetime.Token); }
            catch (Exception ex) when (ex is not OperationCanceledException) { DesktopLog.Error("Own picture", ex); }
            byte[]? picked = null;
            (int Width, int Height) size = (0, 0);
            var preview = new Border { Width = 160, Height = 160, CornerRadius = new CornerRadius(80), Background = new SolidColorBrush(Microsoft.UI.ColorHelper.FromArgb(0x33, 0x80, 0x88, 0x99)), HorizontalAlignment = HorizontalAlignment.Center };
            AutomationProperties.SetName(preview, "Picture preview");
            var previewImage = new Image { Stretch = Stretch.UniformToFill };
            var scale = new ScaleTransform { CenterX = 80, CenterY = 80 };
            previewImage.RenderTransform = scale;
            preview.Child = previewImage;
            if (own?.DisplayUrl is not null && await avatars.ImageAsync(mailbox.Key, lifetime.Token) is { } current) previewImage.Source = await Bitmap(current);
            var zoom = new Slider { Header = "Zoom", Minimum = 1, Maximum = 4, StepFrequency = 0.1, Value = 1, IsEnabled = false };
            AutomationProperties.SetName(zoom, "Zoom");
            zoom.ValueChanged += (_, _) => scale.ScaleX = scale.ScaleY = zoom.Value;
            var choose = new Button { Content = own is null ? "Choose a picture…" : "Choose a new picture…" };
            AutomationProperties.SetName(choose, "Choose a picture");
            var visibility = new ComboBox { Header = "Who can see it", MinWidth = 200 };
            AutomationProperties.SetName(visibility, "Who can see it");
            foreach (var option in AvatarVisibility.Options) visibility.Items.Add(new ComboBoxItem { Content = option.Label, Tag = option.Value });
            visibility.SelectedIndex = own?.Visibility switch { "everyone" => 0, "nobody" => 2, _ => 1 };
            var all = new CheckBox { Content = "Use for all my mailboxes", Visibility = ToVisibility(ViewModel.Mailboxes.Count > 1) };
            var status = new TextBlock { TextWrapping = TextWrapping.Wrap, FontSize = 12.5 };
            var remove = new Button { Content = "Remove picture", Visibility = ToVisibility(own is not null) };
            choose.Click += async (_, _) =>
            {
                try
                {
                    var file = await PickPictureAsync();
                    if (file is null) return;
                    (picked, size) = await AvatarImages.PrepareAsync(file);
                    previewImage.Source = await Bitmap(picked);
                    zoom.IsEnabled = true;
                    zoom.Value = 1;
                    status.Text = "Choose Save to use this picture.";
                }
                catch (Exception ex) { DesktopLog.Error("Reading a picture failed", ex); status.Text = "This picture could not be read. Choose a JPEG, PNG, WebP or HEIC picture."; }
            };
            remove.Click += async (_, _) =>
            {
                try
                {
                    await postbox.RemoveAvatarAsync(mailbox.Address, lifetime.Token);
                    avatars.Forget([mailbox.Key, mailbox.Address]);
                    previewImage.Source = null;
                    remove.Visibility = Visibility.Collapsed;
                    own = null;
                    status.Text = "Removed. Your initials show instead.";
                }
                catch (Exception ex) when (ex is not OperationCanceledException) { status.Text = (ex as PostboxException)?.Message ?? "Could not remove the picture."; }
            };
            var panel = new StackPanel { Spacing = 12, MinWidth = 360, MaxWidth = 460 };
            panel.Children.Add(new TextBlock { Text = "For " + mailbox.Key, FontFamily = (FontFamily)Application.Current.Resources["MonoFont"], FontSize = 12.5 });
            panel.Children.Add(preview);
            panel.Children.Add(choose);
            panel.Children.Add(zoom);
            panel.Children.Add(visibility);
            panel.Children.Add(new TextBlock { Text = "People I know: your contacts, people you've written to and members of your groups. Copies others already downloaded can't be recalled.", TextWrapping = TextWrapping.Wrap, FontSize = 12 });
            panel.Children.Add(all);
            panel.Children.Add(remove);
            panel.Children.Add(status);
            var dialog = new ContentDialog
            {
                XamlRoot = Root.XamlRoot, RequestedTheme = Root.ActualTheme, Resources = DialogTheme(), Title = "Profile picture",
                Content = new ScrollViewer { Content = panel, MaxHeight = 600 }, PrimaryButtonText = "Save", CloseButtonText = "Close",
            };
            dialog.PrimaryButtonClick += async (_, args) =>
            {
                var deferral = args.GetDeferral();
                try
                {
                    var chosen = (string)((ComboBoxItem)visibility.SelectedItem).Tag;
                    if (picked is null)
                    {
                        if (own is null) { status.Text = "Choose a picture first."; args.Cancel = true; return; }
                        if (chosen != own.Visibility) await postbox.SetAvatarVisibilityAsync(mailbox.Address, chosen, lifetime.Token);
                    }
                    else
                    {
                        var crop = AvatarCrop.Square(size.Width, size.Height, zoom.Value);
                        await postbox.SetAvatarAsync(mailbox.Address, picked, "image/jpeg", crop, chosen, all.IsChecked == true, lifetime.Token);
                    }
                    avatars.Forget(all.IsChecked == true ? ViewModel.Mailboxes.SelectMany(m => new[] { m.Key, m.Address }) : [mailbox.Key, mailbox.Address]);
                    AccountStatus.Text = all.IsChecked == true ? "Saved for all your mailboxes." : "Saved.";
                    RefreshAvatars();
                }
                catch (Exception ex) when (ex is not OperationCanceledException)
                {
                    status.Text = (ex as PostboxException)?.Message ?? "Could not reach your inbox. Please try again.";
                    args.Cancel = true;
                }
                finally { deferral.Complete(); }
            };
            await dialog.ShowAsync();
        }
        finally { dialogOpen = false; }
    }

    private static async Task<BitmapImage> Bitmap(byte[] bytes)
    {
        var image = new BitmapImage();
        using var stream = new Windows.Storage.Streams.InMemoryRandomAccessStream();
        await stream.WriteAsync(bytes.AsBuffer());
        stream.Seek(0);
        await image.SetSourceAsync(stream);
        return image;
    }

    private async Task<Windows.Storage.StorageFile?> PickPictureAsync()
    {
#if UI_TESTS
        var path = Environment.GetEnvironmentVariable("PIGEONPOST_UI_AVATAR_FILE") ?? Path.Combine(AppContext.BaseDirectory, "PreviewFixtures", "preview.png");
        return await Windows.Storage.StorageFile.GetFileFromPathAsync(path);
#else
        var picker = new Windows.Storage.Pickers.FileOpenPicker { SuggestedStartLocation = Windows.Storage.Pickers.PickerLocationId.PicturesLibrary };
        foreach (var type in new[] { ".jpg", ".jpeg", ".png", ".webp", ".heic", ".bmp", ".gif" }) picker.FileTypeFilter.Add(type);
        WinRT.Interop.InitializeWithWindow.Initialize(picker, WinRT.Interop.WindowNative.GetWindowHandle(this));
        return await picker.PickSingleFileAsync();
#endif
    }
}

// Pictures leave the device as a fresh JPEG: decoded by Windows (HEIC and WebP included, EXIF
// orientation applied), at most 2048 px, with no metadata. The server crops and re-encodes again.
internal static class AvatarImages
{
    public static async Task<(byte[] Jpeg, (int Width, int Height) Size)> PrepareAsync(Windows.Storage.StorageFile file)
    {
        if ((await file.GetBasicPropertiesAsync()).Size > 40 * 1024 * 1024) throw new IOException("Picture too large.");
        using var input = await file.OpenReadAsync();
        var decoder = await Windows.Graphics.Imaging.BitmapDecoder.CreateAsync(input);
        var width = (int)decoder.OrientedPixelWidth;
        var height = (int)decoder.OrientedPixelHeight;
        var factor = Math.Min(1.0, 2048.0 / Math.Max(width, height));
        var transform = new Windows.Graphics.Imaging.BitmapTransform
        {
            ScaledWidth = (uint)Math.Max(1, Math.Round(width * factor)), ScaledHeight = (uint)Math.Max(1, Math.Round(height * factor)),
            InterpolationMode = Windows.Graphics.Imaging.BitmapInterpolationMode.Fant,
        };
        // Scaling applies after orientation when the decoder respects EXIF orientation.
        using var pixels = await decoder.GetSoftwareBitmapAsync(Windows.Graphics.Imaging.BitmapPixelFormat.Bgra8, Windows.Graphics.Imaging.BitmapAlphaMode.Ignore,
            transform, Windows.Graphics.Imaging.ExifOrientationMode.RespectExifOrientation, Windows.Graphics.Imaging.ColorManagementMode.ColorManageToSRgb);
        using var output = new Windows.Storage.Streams.InMemoryRandomAccessStream();
        var encoder = await Windows.Graphics.Imaging.BitmapEncoder.CreateAsync(Windows.Graphics.Imaging.BitmapEncoder.JpegEncoderId, output);
        encoder.SetSoftwareBitmap(pixels);
        await encoder.FlushAsync();
        var bytes = new byte[output.Size];
        output.Seek(0);
        using (var reader = new Windows.Storage.Streams.DataReader(output.GetInputStreamAt(0)))
        {
            await reader.LoadAsync((uint)output.Size);
            reader.ReadBytes(bytes);
        }
        return (bytes, (pixels.PixelWidth, pixels.PixelHeight));
    }
}

// Pictures already shown in the app, written where Windows can read them for toasts.
internal static class AvatarFiles
{
    private static readonly string Folder = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Pigeonpost", "avatars");
    private static readonly Dictionary<string, string> Paths = new(StringComparer.Ordinal);

    public static void Remember(string key, byte[] jpeg)
    {
        try
        {
            Directory.CreateDirectory(Folder);
            var name = Convert.ToHexString(System.Security.Cryptography.SHA256.HashData(jpeg))[..24].ToLowerInvariant() + ".jpg";
            var path = Path.Combine(Folder, name);
            if (!File.Exists(path)) File.WriteAllBytes(path, jpeg);
            lock (Paths) Paths[key] = path;
        }
        catch (Exception ex) when (ex is IOException or UnauthorizedAccessException) { }
    }

    public static Uri? For(string? key)
    {
        if (key is null) return null;
        lock (Paths) return Paths.TryGetValue(AvatarDirectory.Key(key), out var path) && File.Exists(path) ? new Uri(path) : null;
    }

    public static void Clear()
    {
        lock (Paths) Paths.Clear();
        try { if (Directory.Exists(Folder)) Directory.Delete(Folder, true); } catch (Exception ex) when (ex is IOException or UnauthorizedAccessException) { }
    }
}

// DPAPI for the signed-in Windows user: the group store opens only for this user on this machine.
internal sealed class DpapiProtector : IStateProtector
{
    public async Task<byte[]> ProtectAsync(byte[] plain)
    {
        var provider = new Windows.Security.Cryptography.DataProtection.DataProtectionProvider("LOCAL=user");
        var sealedBuffer = await provider.ProtectAsync(plain.AsBuffer());
        return sealedBuffer.ToArray();
    }

    public async Task<byte[]> UnprotectAsync(byte[] sealedBytes)
    {
        var provider = new Windows.Security.Cryptography.DataProtection.DataProtectionProvider();
        var plain = await provider.UnprotectAsync(sealedBytes.AsBuffer());
        return plain.ToArray();
    }
}
