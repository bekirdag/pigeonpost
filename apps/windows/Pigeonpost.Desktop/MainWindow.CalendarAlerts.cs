using System.Globalization;
using Pigeonpost.Core;
using Pigeonpost.Core.Calendar;

namespace Pigeonpost.Desktop;

// Audit P2-18 on Windows: calendar invitations and reminders as Windows toasts while the app runs (including from the
// notification area). The account event stream carries locators only; the event is re-read under the mailbox's own
// authorization, and Accept/Decline re-check that it is still a pending invitation before answering.
// Closed-app delivery would need the postbox to send calendar pushes over WNS (it sends calls only today).
public sealed partial class MainWindow
{
    private readonly HashSet<string> calendarAlertsShown = new(StringComparer.Ordinal);

    private async Task HandleCalendarAccountEventAsync(string name, string data)
    {
        if (CalendarAlerts.Parse(name, data) is not { } alert || alert.Filtered || !features.Has(ServerFeatures.Calendar)) return;
        if (ViewModel.Mailboxes.FirstOrDefault(m => m.Address == alert.Mailbox) is not { } mailbox) return;
        // A reconnect can replay the stream; one toast per invitation or reminder occurrence.
        if (!calendarAlertsShown.Add($"{alert.Kind}:{alert.Mailbox}:{alert.EventId}:{alert.OccurrenceId}:{alert.StartsAt}")) return;
        try
        {
            var detail = await postbox.GetCalendarEventAsync(alert.Mailbox, alert.Key, lifetime.Token);
            if (detail.Event.Status == "cancelled") return;
            var preview = preferences.LoadNotificationPreview();
            string title, text; var answerable = false;
            if (alert.Kind == CalendarAlertKind.Reminder)
            {
                var settings = await postbox.GetCalendarSettingsAsync(alert.Mailbox, lifetime.Token);
                title = "Calendar";
                text = CalendarAlerts.ReminderText(detail.Occurrence?.Title ?? detail.Event.Title, settings.Previews == true && preview != NotificationPreview.Hidden,
                    alert.StartsAt ?? detail.Occurrence?.StartsAt ?? detail.Event.StartsAt, DateTimeOffset.UtcNow);
            }
            else
            {
                answerable = CalendarAlerts.Answerable(detail);
                if (!answerable) return;
                title = "Invitation";
                text = preview == NotificationPreview.Hidden ? "You have a new calendar invitation."
                    : $"{detail.Event.Organizer?.Name ?? detail.Event.Organizer?.Handle ?? "Someone"} invited you: {detail.Event.Title ?? "Untitled event"}"
                      + (detail.Event.StartsAt is { } at ? " · " + DateTimeOffset.FromUnixTimeSeconds(at).ToLocalTime().ToString("ddd d MMM, HH:mm", CultureInfo.CurrentCulture) : "");
            }
            if (!DesktopNotifications.AnnounceCalendar(alert, title, text, answerable, ViewModel.Mailboxes.Count > 1 ? mailbox.DisplayName : null))
                DesktopLog.Write("Windows did not accept a calendar alert");
        }
        catch (OperationCanceledException) { }
        catch (Exception ex) { DesktopLog.Write("Calendar alert skipped: " + ex.GetType().Name); }
    }

    public async Task HandleCalendarNotificationAsync(string action, string mailbox, string eventId, string? occurrenceId)
    {
        if (ViewModel.Mailboxes.All(m => m.Address != mailbox) || !features.Has(ServerFeatures.Calendar)) { ShowInbox(); return; }
        var key = new CalendarEventKey(eventId, occurrenceId);
        if (action is "accept" or "decline")
        {
            try
            {
                var detail = await postbox.GetCalendarEventAsync(mailbox, key, lifetime.Token);
                if (!CalendarAlerts.Answerable(detail)) { await OpenCalendarFromAlertAsync(mailbox); return; }
                await postbox.AnswerCalendarInvitationAsync(mailbox, key, action == "accept" ? CalendarAnswer.Accepted : CalendarAnswer.Declined,
                    CalendarAnswerScope.All, null, lifetime.Token);
                await DesktopNotifications.RemoveCalendarAsync(mailbox, eventId);
                AccountStatus.Text = action == "accept" ? $"You accepted “{detail.Event.Title ?? "the invitation"}”." : $"You declined “{detail.Event.Title ?? "the invitation"}”. You can change your answer in Calendar.";
                if (sideView == SideView.Calendar) await RefreshSideViewsAsync();
                return;
            }
            catch (OperationCanceledException) { return; }
            catch (Exception ex)
            {
                DesktopLog.Write("Calendar toast answer failed: " + ex.GetType().Name);
                await OpenCalendarFromAlertAsync(mailbox);
                AccountStatus.Text = "Your answer was not sent. Open the invitation in Calendar to try again.";
                return;
            }
        }
        await OpenCalendarFromAlertAsync(mailbox);
    }

    private Task OpenCalendarFromAlertAsync(string mailbox)
    {
        ShowInbox();
        if (ViewModel.SelectedMailbox?.Address != mailbox)
        {
            var named = ViewModel.Mailboxes.FirstOrDefault(m => m.Address == mailbox)?.DisplayName ?? mailbox;
            AccountStatus.Text = $"This is in {named}. Switch to that inbox to see it in Calendar.";
            return Task.CompletedTask;
        }
        if (calendarNav is not null) ShowSideView(SideView.Calendar);
        return Task.CompletedTask;
    }
}
