using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Pigeonpost.Core.Calendar;

namespace Pigeonpost.Desktop;

internal sealed partial class CalendarPage
{
    private static ContentDialog Form(string title, StackPanel fields, string primary = "Save") => new()
    {
        Title = title, PrimaryButtonText = primary, CloseButtonText = "Cancel", DefaultButton = ContentDialogButton.Primary,
        Content = new ScrollViewer { Content = new ContentControl { Content = fields, HorizontalContentAlignment = HorizontalAlignment.Stretch }, MaxHeight = 460, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled }
    };
    private static void EnableFormFields(ContentDialog form, bool enabled)
    { if (form.Content is ScrollViewer { Content: ContentControl host }) host.IsEnabled = enabled; }
    private async Task NewCalendar(CalendarActionTarget target)
    {
        var name = Field("Calendar name", ""); var color = Choice("Color", ["blue", "green", "purple", "red", "orange", "gray"]);
        var error = Text(""); var fields = new StackPanel { Spacing = 12, MinWidth = 280, Children = { name, color, error } }; var form = Form("New calendar", fields, "Create calendar");
        form.PrimaryButtonClick += async (sender, args) =>
        {
            args.Cancel = true; var deferred = args.GetDeferral(); form.IsPrimaryButtonEnabled = false; EnableFormFields(form, false);
            try
            {
                if (string.IsNullOrWhiteSpace(name.Text) || name.Text.Length > 100) { error.Text = "Enter a calendar name of at most 100 characters."; return; }
                var result = await model.CreateAsync(target, CalendarAttemptKind.Calendar, CalendarWritePayload.From(new { name = name.Text.Trim(), color = color.SelectedItem as string ?? "blue", request_key = "calendar_" + Guid.NewGuid().ToString("N") }), lifetime);
                if (result is not null) form.Hide(); else error.Text = model.State.Error ?? "Creation was not confirmed. Close this form to review the pending request.";
            }
            catch (OperationCanceledException) { }
            catch (Exception) { error.Text = "Could not create the calendar. Your request has not been sent again."; }
            finally { EnableFormFields(form, true); form.IsPrimaryButtonEnabled = target.IsCurrent && model.State.CanCreate && !model.State.Pending.Any(p => p.Kind == CalendarAttemptKind.Calendar); deferred.Complete(); }
        };
        await Show(target, form);
    }
    private async Task Recovery(CalendarActionTarget target, PreparedCalendarAttempt pending)
    {
        var fields = pending.Payload.Fields();
        var title = fields.TryGetProperty("title", out var named) || fields.TryGetProperty("name", out named) ? named.GetString() : null;
        var body = new StackPanel { Spacing = 10, Children = { Text("The server may already have received this request. Retrying uses the exact saved request."),
            Text(title ?? "Saved calendar request", 16), Text("Inbox: " + model.State.Mailbox), Text("Discard removes this local request. It does not undo a delivered event or calendar.") } };
        var result = await Show(target, new ContentDialog { Title = "Review pending request", Content = body, PrimaryButtonText = "Retry saved request", SecondaryButtonText = "Discard locally", CloseButtonText = "Keep pending" });
        if (!target.IsCurrent) return;
        if (result == ContentDialogResult.Primary) await model.RetryAsync(target, pending, lifetime);
        else if (result == ContentDialogResult.Secondary)
        {
            if (await Show(target, new ContentDialog { Title = "Discard this saved request?", Content = Text("This only removes recovery on this device. A calendar or event already created on the server will remain."), PrimaryButtonText = "Discard locally", CloseButtonText = "Keep pending" }) == ContentDialogResult.Primary)
                await model.DiscardAsync(target, pending, lifetime);
        }
    }
    private async Task Detail(CalendarActionTarget target, CalendarEventKey key)
    {
        var detail = await model.OpenEventAsync(target, key, lifetime); if (detail is null || !target.IsCurrent) return;
        var e = detail.Event; var o = detail.Occurrence;
        var fields = new StackPanel { Spacing = 12, MinWidth = 280 };
        fields.Children.Add(Heading(o?.Title ?? e.Title ?? "Untitled event"));
        fields.Children.Add(Text(e.Status == "cancelled" || o?.Status == "cancelled" ? "Cancelled" : ""));
        var day = o?.StartDate ?? e.StartDate;
        if ((o?.AllDay ?? e.AllDay) == true)
        {
            var end = o?.EndDate ?? e.EndDate; string? last = null;
            try { if (end is not null) last = CalendarDay.Parse(end).PlusDays(-1).Text; } catch (ArgumentException) { }
            fields.Children.Add(Text("All day · " + day + (last is null || last == day ? "" : " – " + last)));
        }
        else
        {
            var zone = o?.Tzid ?? e.Tzid ?? model.State.DisplayZone;
            string At(string? literal, long? at) => literal ?? (at is null ? "Time unavailable" : NodaTime.Instant.FromUnixTimeSeconds(at.Value).InZone(CalendarTime.Zone(zone)).LocalDateTime.ToDateTimeUnspecified().ToString("yyyy-MM-dd HH:mm"));
            fields.Children.Add(Text(At(o?.Start ?? (o?.StartsAt is null ? e.Start : null), o?.StartsAt ?? e.StartsAt) + " – " + At(o?.End ?? (o?.EndsAt is null ? e.End : null), o?.EndsAt ?? e.EndsAt) + "\n" + zone));
        }
        if (!string.IsNullOrEmpty(o?.Location ?? e.Location)) fields.Children.Add(Text(o?.Location ?? e.Location!));
        if (!string.IsNullOrEmpty(o?.Description ?? e.Description)) fields.Children.Add(new TextBox { Text = o?.Description ?? e.Description ?? "", IsReadOnly = true, TextWrapping = TextWrapping.Wrap, AcceptsReturn = true, Header = "Description" });
        fields.Children.Add(Text("Organizer: " + (e.Organizer?.Name ?? e.Organizer?.Handle ?? e.Organizer?.Address ?? "Unknown")));
        if (e.My?.Rsvp is { } rsvp) fields.Children.Add(Text("Your response: " + rsvp));
        if (e.Attendees is { Count: > 0 })
        {
            var people = new StackPanel { Spacing = 6 };
            foreach (var person in e.Attendees) people.Children.Add(Text((person.Name ?? person.Handle ?? person.Address ?? person.Contact ?? "Guest") + " · " + (person.Rsvp ?? "pending")));
            fields.Children.Add(new Expander { Header = "People", Content = people, HorizontalAlignment = HorizontalAlignment.Stretch });
        }
        var recurring = !string.IsNullOrWhiteSpace(e.Rrule) && key.OccurrenceId is not null;
        var scope = Choice("Apply to", recurring ? ["Choose which events…", "This occurrence", "This and following occurrences", "The whole series"] : ["This event"]);
        if (recurring) fields.Children.Add(scope);
        CalendarEventScope? Scope() => !recurring ? CalendarEventScope.All : scope.SelectedIndex switch { 1 => CalendarEventScope.This, 2 => CalendarEventScope.Following, 3 => CalendarEventScope.All, _ => null };
        Func<Task>? after = null;
        var next = new ContentDialog { Title = "Event details", Content = new ScrollViewer { Content = fields, MaxHeight = 460 }, CloseButtonText = "Done" };
        var feedback = Text("");
        void Command(string label, Func<Task> run, bool needsScope = false)
        {
            var button = Button(label); button.Click += (_, _) => { if (needsScope && Scope() is null) { feedback.Text = "Choose which events this action applies to."; return; } after = run; next.Hide(); }; fields.Children.Add(button);
        }
        if (e.Status == "confirmed" && e.My?.Role == "organizer" && e.CanEdit == true)
        {
            Command("Edit event", async () => { if (Scope() is { } selected && await model.ReviewEditAsync(target, key, selected, lifetime) is { } reviewed) await Edit(target, reviewed); }, true);
            Command("Cancel event", async () => { if (Scope() is { } selected && await model.ReviewEditAsync(target, key, selected, lifetime) is { } reviewed) await Cancel(target, reviewed); }, true);
        }
        if (e.Status == "confirmed" && e.My is { Role: "attendee" or "cohost", AttendeeId: { Length: > 0 } })
        {
            var answerScope = Choice("Respond for", recurring ? ["The whole series", "This occurrence"] : ["This event"]); fields.Children.Add(answerScope);
            foreach (var (label, answer) in new[] { ("Accept", CalendarAnswer.Accepted), ("Maybe", CalendarAnswer.Tentative), ("Decline", CalendarAnswer.Declined) })
                Command(label, async () => { await model.AnswerAsync(target, key, answer, answerScope.SelectedIndex == 1 ? CalendarAnswerScope.This : CalendarAnswerScope.All, ct: lifetime); });
        }
        if (e.My?.Role is "organizer" or "attendee" or "cohost")
        {
            Command("Reminders", () => Reminders(target, key, e.Reminders)); Command("Export calendar file", () => Export(target, key));
        }
        fields.Children.Add(feedback); await Show(target, next);
        if (!disposed && target.IsCurrent && after is not null) await after();
    }
    private async Task Cancel(CalendarActionTarget target, CalendarEditTarget reviewed)
    {
        var scope = reviewed.Scope switch { CalendarEventScope.This => "this occurrence", CalendarEventScope.Following => "this and following occurrences", _ => string.IsNullOrEmpty(reviewed.Response.Event.Rrule) ? "this event" : "the whole series" };
        var message = Field("Message to invitees (optional)", "");
        if (await Show(target, new ContentDialog { Title = "Cancel " + scope + "?", Content = new StackPanel { Spacing = 12, Children = { Text(reviewed.Response.Event.Title ?? "Untitled event"), message } }, PrimaryButtonText = "Cancel event", CloseButtonText = "Keep event" }) == ContentDialogResult.Primary)
            await model.CancelEventAsync(target, reviewed, message.Text, lifetime);
    }
    private async Task Reminders(CalendarActionTarget target, CalendarEventKey key, IReadOnlyList<CalendarReminder>? current)
    {
        var input = Field("Minutes before (separate with commas)", string.Join(", ", current?.Select(r => r.MinutesBefore) ?? []));
        var error = Text(""); var form = Form("Your reminders", new() { Spacing = 12, Children = { input, Text("Only your own reminders change. Leave empty for no reminders."), error } });
        form.PrimaryButtonClick += async (sender, args) =>
        {
            args.Cancel = true; var deferred = args.GetDeferral(); form.IsPrimaryButtonEnabled = false; EnableFormFields(form, false);
            try { var reminders = CalendarReminderInput.Parse(input.Text).Select(n => new CalendarReminder(n, "push")).ToArray(); if (await model.SaveRemindersAsync(target, key, reminders, lifetime)) form.Hide(); else error.Text = model.State.Error ?? "The reminders were not confirmed."; }
            catch (ArgumentException exception) { error.Text = exception.Message; }
            catch (OperationCanceledException) { }
            finally { EnableFormFields(form, true); form.IsPrimaryButtonEnabled = target.IsCurrent; deferred.Complete(); }
        };
        await Show(target, form);
    }
    private async Task Settings(CalendarActionTarget target)
    {
        var state = model.State; var zone = Field("Time zone", state.Settings.Timezone ?? state.DisplayZone);
        var week = Choice("First day of week", System.Globalization.CultureInfo.CurrentCulture.DateTimeFormat.DayNames, state.WeekStart);
        var previews = new CheckBox { Content = "Show event details in reminder notifications", IsChecked = state.Settings.Previews == true };
        var error = Text(""); var form = Form("Calendar settings", new() { Spacing = 12, MinWidth = 280, Children = { zone, week, previews, error } });
        form.PrimaryButtonClick += async (sender, args) =>
        {
            args.Cancel = true; var deferred = args.GetDeferral(); form.IsPrimaryButtonEnabled = false; EnableFormFields(form, false);
            try
            {
                _ = CalendarTime.Zone(zone.Text.Trim());
                if (await model.SaveSettingsAsync(target, CalendarWritePayload.From(new { timezone = zone.Text.Trim(), week_start = week.SelectedIndex, previews = previews.IsChecked == true }), lifetime) is not null) form.Hide();
                else error.Text = model.State.Error ?? "The settings were not confirmed.";
            }
            catch (ArgumentException exception) { error.Text = exception.Message; }
            catch (OperationCanceledException) { }
            finally { EnableFormFields(form, true); form.IsPrimaryButtonEnabled = target.IsCurrent; deferred.Complete(); }
        };
        await Show(target, form);
    }
    private async Task Export(CalendarActionTarget target, CalendarEventKey key)
    {
        var bytes = await model.ExportAsync(target, key, lifetime); if (bytes is null) return;
        try
        {
            if (!target.IsCurrent || disposed) return;
            var picker = new Windows.Storage.Pickers.FileSavePicker { SuggestedFileName = "Pigeonpost event" };
            picker.FileTypeChoices.Add("Calendar file", new List<string> { ".ics" }); WinRT.Interop.InitializeWithWindow.Initialize(picker, window);
            var file = await picker.PickSaveFileAsync();
            if (file is not null && target.IsCurrent && !disposed) await Windows.Storage.FileIO.WriteBytesAsync(file, bytes);
        }
        finally { System.Security.Cryptography.CryptographicOperations.ZeroMemory(bytes); }
    }
}
