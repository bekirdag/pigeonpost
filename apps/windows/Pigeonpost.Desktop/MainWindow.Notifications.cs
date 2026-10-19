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

    private void InitializeNotifications()
    {
        try { notifications = new(WinRT.Interop.WindowNative.GetWindowHandle(this), ShowInbox, Quit); }
        catch (Exception ex)
        {
            notificationInitializationError = ex;
            DesktopLog.Error("Notification-area icon unavailable", ex);
            AccountStatus.Text = "The notification-area icon is unavailable. Keep this window open for new-message alerts.";
        }
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
            if (!visible && !DesktopNotifications.Announce(arrival))
                AccountStatus.Text = "New messages arrived. Check Windows notification settings if alerts are disabled.";
        }
        notifications?.Update(unreadState.Count);
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
                foreach (var arrival in arrivals) DesktopNotifications.Announce(arrival);
                notifications?.Update(unreadState.Count);
            }
        }
        catch (Exception ex) { DesktopLog.Error("Checking other mailboxes failed", ex); /* Retain the last count; retry next refresh. */ }
        finally { refreshingNotifications = false; }
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
        }
        finally { openingNotification = false; pendingNotification = null; }
        await AcknowledgeVisibleAsync();
    }
}
