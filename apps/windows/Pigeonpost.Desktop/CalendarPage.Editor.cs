using System.Globalization;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Pigeonpost.Core.Calendar;

namespace Pigeonpost.Desktop;

internal sealed partial class CalendarPage
{
    private static (ComboBox Control, Func<string> Value) Policy(string label, string[] labels, string[] values, string original)
    {
        var selected = Array.IndexOf(values, original);
        var box = Choice(label, selected < 0 ? labels.Concat(["Keep current setting"]).ToArray() : labels, selected < 0 ? values.Length : selected);
        return (box, () => box.SelectedIndex >= 0 && box.SelectedIndex < values.Length ? values[box.SelectedIndex] : original);
    }
    private async Task Edit(CalendarActionTarget target, CalendarEditTarget? reviewed = null)
    {
        var state = model.State; CalendarEventDraft initial;
        try { initial = reviewed is null ? new(state.ContextId, state.SelectedDay.Text, state.SelectedDay.Text, state.DisplayZone) : CalendarEventDraft.From(reviewed, state.DisplayZone); }
        catch (ArgumentException) { await Problem(target, "This event cannot be edited on this device. Refresh its details and try again."); return; }
        var title = Field("Event title", initial.Title);
        var description = Field("Description", initial.Description); description.AcceptsReturn = true; description.TextWrapping = TextWrapping.Wrap; description.MaxHeight = 140;
        var location = Field("Location", initial.Location);
        var allDay = new CheckBox { Content = "All day", IsChecked = initial.AllDay };
        var first = Field("Start date (YYYY-MM-DD)", initial.StartDay); var last = Field("Last date (YYYY-MM-DD)", initial.LastDay);
        var start = Field("Start time (HH:mm)", initial.StartTime); var end = Field("End time (HH:mm)", initial.EndTime); var zone = Field("Time zone", initial.Tzid);
        var fold = Choice("If clocks repeat this time", ["Choose when needed", "Earlier occurrence", "Later occurrence"], initial.Fold switch { CalendarFold.Earlier => 1, CalendarFold.Later => 2, _ => 0 });
        var calendars = state.Folders.ToArray(); var calendar = Choice("Calendar", new[] { "Default calendar" }.Concat(calendars.Select(c => c.Name ?? "Calendar")).ToArray(), Array.FindIndex(calendars, c => c.Id == initial.CalendarId) + 1);
        var recurrence = Field("Repeat rule (optional)", initial.Rrule); recurrence.PlaceholderText = "For example: FREQ=WEEKLY;BYDAY=MO";
        var handles = Field("Invite Pigeonpost handles (one per line)", ""); handles.AcceptsReturn = true; handles.MaxHeight = 120;
        var contacts = Field("Invite guest contacts (one per line)", ""); contacts.AcceptsReturn = true; contacts.MaxHeight = 120;
        var reminders = Field("Reminder minutes before (separate with commas)", initial.ReminderText);
        var meeting = new CheckBox { Content = "Add a Pigeonpost meeting", IsChecked = initial.Meeting.Enabled };
        var (transport, readTransport) = Policy("Meeting size", ["Small call · up to 6 people", "Large call · up to 30 people"], ["mesh", "sfu"], initial.Meeting.Transport);
        var (lobby, readLobby) = Policy("Who waits in the lobby", ["Guests", "Everyone except hosts", "No invited participants"], ["guests", "everyone_except_hosts", "none"], initial.Meeting.Waiting);
        var (admit, readAdmit) = Policy("Who can admit people", ["Organizer and cohosts", "Organizer", "Hosts and selected attendees"], ["hosts", "organizer", "delegated"], initial.Meeting.Admit);
        var admitters = Field("Other people who can admit (one handle or attendee ID per line)", string.Join("\n", initial.Meeting.Admitters ?? [])); admitters.AcceptsReturn = true;
        var joinBefore = Field("Open the meeting minutes before (0–10)", initial.Meeting.JoinBeforeMinutes.ToString(CultureInfo.InvariantCulture));
        var knock = new CheckBox { Content = "Allow uninvited people to request entry", IsChecked = initial.Meeting.AllowKnock };
        var people = new CheckBox { Content = "Guests can see the participant list", IsChecked = initial.Meeting.GuestsSeeList };
        var policies = new StackPanel { Spacing = 10, Children = { transport, lobby, admit, admitters, joinBefore, knock, people } };
        var (privacy, readPrivacy) = Policy("Event privacy", ["Default", "Private"], ["default", "private"], initial.Privacy);
        var (transparency, readTransparency) = Policy("Availability", ["Busy", "Free"], ["opaque", "transparent"], initial.Transparency);
        var (locale, readLocale) = Policy("Guest invitation language", ["English", "Turkish"], ["en", "tr"], initial.Locale);
        var advanced = new StackPanel { Spacing = 10, Children = { recurrence, privacy, transparency, locale } };
        var fields = new StackPanel { Spacing = 12, MinWidth = 280, Children = { title, allDay, first, last, start, end, zone, fold, location, description } };
        if (!initial.IsOccurrenceEdit)
        {
            fields.Children.Add(calendar); fields.Children.Add(meeting); fields.Children.Add(policies);
            fields.Children.Add(new Expander { Header = "Repeat and privacy", Content = advanced, HorizontalAlignment = HorizontalAlignment.Stretch });
        }
        if (reviewed is null)
        {
            fields.Children.Add(handles); if (state.EmailEnabled) fields.Children.Add(contacts); fields.Children.Add(reminders);
        }
        var error = Text(""); fields.Children.Add(error);
        void UpdateInputs()
        {
            var timed = allDay.IsChecked != true; start.IsEnabled = end.IsEnabled = zone.IsEnabled = fold.IsEnabled = timed;
            policies.Visibility = meeting.IsChecked == true ? Visibility.Visible : Visibility.Collapsed;
            admitters.Visibility = readAdmit() == "delegated" ? Visibility.Visible : Visibility.Collapsed;
        }
        allDay.Click += (_, _) => UpdateInputs(); meeting.Click += (_, _) => UpdateInputs(); admit.SelectionChanged += (_, _) => UpdateInputs(); UpdateInputs();
        var form = Form(reviewed is null ? "New event" : "Edit event", fields, reviewed is null ? "Create event" : "Save changes");
        form.PrimaryButtonClick += async (sender, args) =>
        {
            args.Cancel = true; var deferred = args.GetDeferral(); form.IsPrimaryButtonEnabled = false; EnableFormFields(form, false);
            try
            {
                if (!target.IsCurrent || model.State.ContextId != initial.ContextId) return;
                var changedMeeting = initial.Meeting;
                if (!initial.IsOccurrenceEdit)
                {
                    if (!int.TryParse(joinBefore.Text.Trim(), NumberStyles.None, CultureInfo.InvariantCulture, out var minutes)) throw new ArgumentException("Choose a meeting opening time between 0 and 10 minutes.");
                    changedMeeting = initial.Meeting with { Enabled = meeting.IsChecked == true, Transport = readTransport(), Waiting = readLobby(),
                        Admit = readAdmit(), Admitters = admitters.Text.Split(['\r', '\n'], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries), JoinBeforeMinutes = minutes, AllowKnock = knock.IsChecked == true, GuestsSeeList = people.IsChecked == true };
                }
                static string[] Lines(string text) => text.Split(['\r', '\n'], StringSplitOptions.RemoveEmptyEntries | StringSplitOptions.TrimEntries);
                var invited = Lines(handles.Text); if (invited.Any(value => !value.StartsWith('/'))) throw new ArgumentException("Use Pigeonpost handles beginning with / in the handle list.");
                var draft = initial with { Title = title.Text, Description = description.Text, Location = location.Text, AllDay = allDay.IsChecked == true,
                    StartDay = first.Text.Trim(), LastDay = last.Text.Trim(), StartTime = start.Text.Trim(), EndTime = end.Text.Trim(), Tzid = zone.Text.Trim(),
                    Fold = fold.SelectedIndex switch { 1 => CalendarFold.Earlier, 2 => CalendarFold.Later, _ => null },
                    CalendarId = calendar.SelectedIndex > 0 ? calendars[calendar.SelectedIndex - 1].Id : initial.CalendarId,
                    Rrule = recurrence.Text, Privacy = readPrivacy(), Transparency = readTransparency(), Locale = readLocale(),
                    Meeting = changedMeeting, ReminderText = reminders.Text,
                    Invitees = invited.Concat(state.EmailEnabled ? Lines(contacts.Text) : []).Select(value => new CalendarInviteeDraft(value)).ToArray() };
                _ = draft.Fields();
                var confirmed = reviewed is null
                    ? await model.CreateAsync(target, CalendarAttemptKind.Event, draft.CreatePayload("event_" + Guid.NewGuid().ToString("N")), lifetime) is not null
                    : await model.UpdateEventAsync(target, draft, lifetime) is not null;
                if (confirmed) form.Hide(); else error.Text = model.State.Error ?? "The change was not confirmed. Your form is kept. Close it to review any pending request.";
            }
            catch (ArgumentException exception) { error.Text = exception.Message; }
            catch (OperationCanceledException) { }
            catch (Exception) { error.Text = "The change was not confirmed. Your form is kept. Refresh the event before trying again."; }
            finally
            {
                form.IsPrimaryButtonEnabled = target.IsCurrent && !model.State.Busy && (reviewed is not null || model.State.CanCreate && !model.State.Pending.Any(p => p.Kind == CalendarAttemptKind.Event));
                EnableFormFields(form, true);
                deferred.Complete();
            }
        };
        await Show(target, form);
    }
}
