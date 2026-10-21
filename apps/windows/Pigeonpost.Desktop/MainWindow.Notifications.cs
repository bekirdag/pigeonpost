using Microsoft.UI.Windowing;
using Microsoft.UI.Xaml;
using Pigeonpost.Core;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow
{
    private readonly DesktopUnreadState unreadState = new();
    private DesktopNotifications? notifications;
    private Exception? notificationInitializationError;
    private bool activeWindow;
    private bool quitting;
    private bool acknowledging;
    private bool refreshingNotifications;
    private MessageArrival? pendingNotification;
    private bool openingNotification;
    private CancellationTokenSource? accountEvents;

    private void StartAccountEvents()
    {
#if !UI_TESTS
        accountEvents?.Cancel();
        accountEvents?.Dispose();
        accountEvents = CancellationTokenSource.CreateLinkedTokenSource(lifetime.Token);
        var cancellation = accountEvents.Token;
        var model = ViewModel;
        _ = WatchAsync();
        async Task WatchAsync()
        {
            try
            {
                await postbox.WatchMailAsync(log: DesktopLog.Write, changed: () =>
                {
                    var completion = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
                    if (!DispatcherQueue.TryEnqueue(async () =>
                    {
                        try
                        {
                            if (cancellation.IsCancellationRequested || !ReferenceEquals(model, ViewModel)) return;
                            await model.RefreshAsync(background: true);
                            await RefreshOtherNotificationsAsync();
                        }
                        finally { completion.TrySetResult(); }
                    })) completion.TrySetResult();
                    return completion.Task;
                }, cancellationToken: cancellation);
            }
            catch (OperationCanceledException) { }
        }
#endif
    }

    private bool ReadingVisibleSubject() => activeWindow && AppWindow.IsVisible && !dialogOpen && !openingNotification && pendingNotification is null
        && InboxLayout.Visibility == Visibility.Visible && MessagingArea.Visibility == Visibility.Visible
        && AppWindow.Presenter is OverlappedPresenter { State: not OverlappedPresenterState.Minimized };

    // Called by App once this process is the single running instance: a second launch that only
    // hands over must never add (and with the stable identity, replace) the running app's icon.
    public void InitializeNotifications()
    {
        try { notifications = new(WinRT.Interop.WindowNative.GetWindowHandle(this), ShowInbox, Quit); }
        catch (Exception ex)
        {
            notificationInitializationError = ex;
            DesktopLog.Error("Notification-area icon unavailable", ex);
            AccountStatus.Text = "The notification-area icon is unavailable. Keep this window open for new-message alerts.";
        }
        if (notifications is { CanHide: false })
            AccountStatus.Text = "The notification-area icon is unavailable. Keep this window open for new-message alerts.";
        Activated += async (_, e) =>
        {
            activeWindow = e.WindowActivationState != WindowActivationState.Deactivated;
            if (activeWindow)
            {
                notifications?.Update(unreadState.Count, force: true);
                await AcknowledgeVisibleAsync();
            }
        };
        AppWindow.Closing += (_, e) =>
        {
            if (quitting || notifications?.CanHide != true || InboxLayout.Visibility != Visibility.Visible) return;
            e.Cancel = true;
            activeWindow = false;
            AppWindow.Hide();
            ShowTrayHintOnce();
        };
    }

    public void ShowInbox()
    {
        AppWindow.Show();
        if (AppWindow.Presenter is OverlappedPresenter presenter) presenter.Restore();
        Activate();
    }

    private void Quit() { quitting = true; Close(); }

    private async Task AcknowledgeVisibleAsync()
    {
        if (acknowledging || !ReadingVisibleSubject()) return;
        acknowledging = true;
        try { await ViewModel.AcknowledgeSelectedAsync(ReadingVisibleSubject); }
        finally { acknowledging = false; }
    }

    private void Inbox_Updated(object? sender, InboxUpdate update)
    {
        if (!ReferenceEquals(sender, ViewModel)) return;
        var arrivals = unreadState.Observe(update.Mailbox.Address, update.Snapshot.Messages);
        if (arrivals.Count > 0) DesktopLog.Write($"{arrivals.Count} new message(s) in the selected mailbox; window visible {AppWindow.IsVisible}, active {activeWindow}");
        foreach (var arrival in arrivals)
        {
            var peer = ConversationBuilder.ResolvePeer(arrival.Peer, update.Snapshot, ViewModel.Mailboxes);
            var visible = ReadingVisibleSubject() && ViewModel.SelectedMailbox?.Address == arrival.Mailbox
                && ViewModel.SelectedConversation?.Peer == peer
                && (ViewModel.SelectedSubject?.Id == arrival.ThreadId || string.IsNullOrEmpty(arrival.ThreadId) && ViewModel.SelectedSubject?.IsDefault == true);
            if (!visible && !Announce(arrival, update.Snapshot))
                AccountStatus.Text = "New messages arrived. Check Windows notification settings if alerts are disabled.";
        }
        notifications?.Update(unreadState.Count);
        SideViewsInboxUpdated();
        _ = AcknowledgeVisibleAsync();
    }

    // The selected mailbox already refreshes its full model. Other mailboxes need only their
    // inbox rows for account-wide notifications, without moving the user's selection or drafts.
    private async Task RefreshOtherNotificationsAsync()
    {
        if (refreshingNotifications) return;
        refreshingNotifications = true;
        var model = ViewModel;
        try
        {
            foreach (var mailbox in model.Mailboxes.Where(m => m.Address != model.SelectedMailbox?.Address))
            {
#if UI_TESTS
                var messages = (await preview.LoadAsync(mailbox.Address, lifetime.Token)).Messages;
#else
                var messages = (await postbox.GetInboxAsync(mailbox.Address, cancellationToken: lifetime.Token)).Messages ?? [];
#endif
                if (lifetime.IsCancellationRequested || !ReferenceEquals(model, ViewModel)) return;
                var arrivals = unreadState.Observe(mailbox.Address, messages);
                if (arrivals.Count > 0) DesktopLog.Write($"{arrivals.Count} new message(s) in another mailbox");
#if UI_TESTS
                var known = await preview.LoadAsync(mailbox.Address, lifetime.Token);
#else
                var known = InboxSnapshot.Empty with { Messages = messages };
#endif
                foreach (var arrival in arrivals) Announce(arrival, known);
                notifications?.Update(unreadState.Count);
            }
        }
        catch (Exception ex) { DesktopLog.Error("Checking other mailboxes failed", ex); /* Retain the last count; retry next refresh. */ }
        finally { refreshingNotifications = false; }
    }

    private bool Announce(MessageArrival arrival, InboxSnapshot snapshot)
    {
        var alert = MessageAlerts.Compose(arrival, snapshot, ViewModel.Mailboxes, preferences.LoadNotificationPreview());
        var mailbox = ViewModel.Mailboxes.FirstOrDefault(m => m.Address == arrival.Mailbox);
        // Name the receiving mailbox only when there is more than one to tell apart.
        return DesktopNotifications.Announce(arrival, alert, ViewModel.Mailboxes.Count > 1 ? mailbox?.DisplayName : null);
    }

    // Reply and Mark as read run from the toast while the window stays hidden or minimised. If the
    // account is not loaded yet (a toast that launched the app), the conversation opens instead.
    public async Task HandleNotificationActionAsync(string action, MessageArrival target, string? reply)
    {
        if (ViewModel.Mailboxes.All(m => m.Address != target.Mailbox)) { await OpenNotificationAsync(target); return; }
        try
        {
            if (action == "reply" && !string.IsNullOrWhiteSpace(reply))
            {
                await ViewModel.ReplyFromAlertAsync(target.Mailbox, target.Peer, target.ThreadId, reply, lifetime.Token);
                DesktopLog.Write("Replied from a Windows notification");
            }
            else if (action == "read")
            {
                var count = await ViewModel.MarkConversationReadAsync(target.Mailbox, target.Peer, lifetime.Token);
                DesktopLog.Write($"Marked {count} message(s) read from a Windows notification");
            }
            else { await OpenNotificationAsync(target); return; }
            await DesktopNotifications.RemoveConversationAsync(target.Mailbox, target.Peer);
            await RefreshUnreadAfterActionAsync(target.Mailbox);
        }
        catch (Exception ex) when (ex is not OperationCanceledException)
        {
            DesktopLog.Error("A notification action failed", ex);
            // Do not lose a typed reply: open the conversation with it in the composer.
            await OpenNotificationAsync(target);
            if (action == "reply" && !string.IsNullOrWhiteSpace(reply) && string.IsNullOrEmpty(ViewModel.Draft)) ViewModel.Draft = reply;
            AccountStatus.Text = "Pigeonpost could not finish that from the notification. Please check this conversation.";
        }
    }

    private async Task RefreshUnreadAfterActionAsync(string mailbox)
    {
        if (ViewModel.SelectedMailbox?.Address == mailbox) return; // RefreshAsync already updated the count.
#if UI_TESTS
        var messages = (await preview.LoadAsync(mailbox, lifetime.Token)).Messages;
#else
        var messages = (await postbox.GetInboxAsync(mailbox, cancellationToken: lifetime.Token)).Messages ?? [];
#endif
        unreadState.Observe(mailbox, messages);
        notifications?.Update(unreadState.Count);
    }

    public async Task OpenNotificationAsync(MessageArrival target)
    {
        pendingNotification = target;
        ShowInbox();
        if (ViewModel.Mailboxes.Count == 0) { pendingNotification = target; return; }
        var mailbox = ViewModel.Mailboxes.FirstOrDefault(m => m.Address == target.Mailbox);
        if (mailbox is null) { pendingNotification = null; return; } // Never cross accounts or trust arbitrary activation parameters.
        openingNotification = true;
        try
        {
        await ViewModel.SwitchMailboxAsync(mailbox);
        await ViewModel.RefreshAsync();
        if (ViewModel.SelectedMailbox?.Address != target.Mailbox) return;
        ViewModel.SenderSearch = "";
        ViewModel.ShowArchived = false;
        var conversation = ViewModel.Conversations.FirstOrDefault(c => c.Messages.Any(m => m.Id == target.MessageId));
        if (conversation is null)
        {
            ViewModel.ShowArchived = true;
            conversation = ViewModel.Conversations.FirstOrDefault(c => c.Messages.Any(m => m.Id == target.MessageId));
        }
        if (conversation is null) { ViewModel.ShowArchived = false; return; }
        ViewModel.SelectConversation(conversation);
        ViewModel.SelectSubject(ViewModel.Subjects.FirstOrDefault(s => s.Messages.Any(m => m.Id == target.MessageId)));
        ShowConversation();
        _ = DesktopNotifications.RemoveConversationAsync(target.Mailbox, conversation.Peer);
        }
        finally { openingNotification = false; pendingNotification = null; }
        await AcknowledgeVisibleAsync();
    }
}

public sealed partial class MainWindow
{
    // Windows owns the alert switch; say so instead of failing silently.
    private void ExplainAlertSetting()
    {
        var setting = Microsoft.Windows.AppNotifications.AppNotificationSetting.Unsupported;
        try { setting = Microsoft.Windows.AppNotifications.AppNotificationManager.Default.Setting; }
        catch (Exception ex) { DesktopLog.Error("Reading the Windows alert setting failed", ex); }
        DesktopLog.Write($"Inbox open; alerts {setting}; notification-area icon {(notifications?.CanHide == true ? "shown" : "unavailable")}");
        if (setting is Microsoft.Windows.AppNotifications.AppNotificationSetting.DisabledForApplication
            or Microsoft.Windows.AppNotifications.AppNotificationSetting.DisabledForUser)
            AccountStatus.Text = "Windows notifications are off for Pigeonpost. Turn them on in Settings > System > Notifications.";
        else if (setting is Microsoft.Windows.AppNotifications.AppNotificationSetting.DisabledByGroupPolicy)
            AccountStatus.Text = "Windows notifications are turned off by your organization's policy.";
    }
}

public sealed partial class MainWindow
{
    private bool trayHintLogged;

    // Once ever: explain that closing kept Pigeonpost running beside the clock.
    private void ShowTrayHintOnce()
    {
        var setting = Microsoft.Windows.AppNotifications.AppNotificationSetting.Unsupported;
        try { setting = Microsoft.Windows.AppNotifications.AppNotificationManager.Default.Setting; }
        catch (Exception ex) { DesktopLog.Error("Reading the Windows alert setting failed", ex); }
        var action = TrayHint.Decide(notifications?.CanHide == true,
            setting == Microsoft.Windows.AppNotifications.AppNotificationSetting.Enabled, preferences.LoadTrayHintShown(), trayHintLogged);
        if (action == TrayHintAction.Show && DesktopNotifications.ShowTrayHint())
        {
            try { preferences.MarkTrayHintShown(); }
            catch (Exception ex) when (ex is IOException or UnauthorizedAccessException) { DesktopLog.Error("Saving the hint state failed", ex); }
            DesktopLog.Write("Notification-area hint shown");
        }
        else if (action != TrayHintAction.None)
        {
            trayHintLogged = true;
            DesktopLog.Write($"Notification-area hint not shown: Windows alerts {setting}");
        }
    }
}
