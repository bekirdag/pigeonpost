using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
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
        TextBlock? feedbackBlock = null; void feedbackText(string message) { if (feedbackBlock is not null) feedbackBlock.Text = message; }
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
            var organizerView = e.My?.Role == "organizer" && e.Status == "confirmed";
            foreach (var person in e.Attendees)
            {
                var shown = person.Kind == "external" && organizerView && person.Contact is { } contact
                    ? (person.Name is { } n && n != contact ? $"{n} ({contact})" : contact) : person.Name ?? person.Handle ?? person.Address ?? person.Contact ?? "Guest";
                people.Children.Add(Text(shown + " · " + (person.Rsvp ?? "pending")));
                // P3-7: when invitation delivery is off (or did not arrive), the organizer can hand a guest their own link.
                if (organizerView && person.Kind == "external" && person.AttendeeId is { Length: > 0 } attendee)
                {
                    var copy = Button("Copy invitation link", "Copy invitation link for " + shown);
                    var linkBox = new TextBox { IsReadOnly = true, Visibility = Visibility.Collapsed, Header = "Invitation link (for this guest only)" };
                    AutomationProperties.SetName(linkBox, "Invitation link for " + shown);
                    copy.Click += async (_, _) =>
                    {
                        copy.IsEnabled = false;
                        try
                        {
                            if (await model.GuestLinkAsync(target, key, attendee, lifetime) is { } link && target.IsCurrent && !disposed)
                            {
                                var package = new Windows.ApplicationModel.DataTransfer.DataPackage(); package.SetText(link.Url);
                                Windows.ApplicationModel.DataTransfer.Clipboard.SetContent(package);
                                linkBox.Text = link.Url; linkBox.Visibility = Visibility.Visible;
                                feedbackText("Invitation link copied. It works for this guest only.");
                            }
                            else feedbackText(model.State.Error ?? "The invitation link could not be created.");
                        }
                        finally { copy.IsEnabled = true; }
                    };
                    people.Children.Add(copy); people.Children.Add(linkBox);
                }
            }
            fields.Children.Add(new Expander { Header = "People", Content = people, HorizontalAlignment = HorizontalAlignment.Stretch });
        }
        var recurring = !string.IsNullOrWhiteSpace(e.Rrule) && key.OccurrenceId is not null;
        var scope = Choice("Apply to", recurring ? ["Choose which events…", "This occurrence", "This and following occurrences", "The whole series"] : ["This event"]);
        if (recurring) fields.Children.Add(scope);
        CalendarEventScope? Scope() => !recurring ? CalendarEventScope.All : scope.SelectedIndex switch { 1 => CalendarEventScope.This, 2 => CalendarEventScope.Following, 3 => CalendarEventScope.All, _ => null };
        Func<Task>? after = null;
        var next = new ContentDialog { Title = "Event details", Content = new ScrollViewer { Content = fields, MaxHeight = 460 }, CloseButtonText = "Done" };
        var feedback = Text(""); feedbackBlock = feedback;
        void Command(string label, Func<Task> run, bool needsScope = false)
        {
            var button = Button(label); button.Click += (_, _) => { if (needsScope && Scope() is null) { feedback.Text = "Choose which events this action applies to."; return; } after = run; next.Hide(); }; fields.Children.Add(button);
        }
        if (e.Meeting?.Code is { } meetingCode && CalendarMeetingCode.Parse(meetingCode) is not null && e.Status == "confirmed"
            && e.My?.Role is "organizer" or "attendee" or "cohost" && (o?.MyRsvp ?? e.My.Rsvp) != "declined")
        {
            if (key.OccurrenceId is null) fields.Children.Add(Text("Choose this meeting from a day's agenda to select its occurrence."));
            else
            {
                var originalContext = model.State.ContextId;
                Command("Join by voice", () => joinMeeting(target, key, meetingCode, false, originalContext));
                Command("Join with video", () => joinMeeting(target, key, meetingCode, true, originalContext));
            }
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

    // Subscription links (feeds): another calendar app can follow one calendar. The link is shown once, when made.
    private async Task Feeds(CalendarActionTarget target)
    {
        var panel = new StackPanel { Spacing = 12, MinWidth = 320 };
        var error = Text("");
        panel.Children.Add(Text("Let another calendar app follow one of your calendars. Anyone with the link sees what it shows; copies already made in other apps can't be recalled."));
        var rows = new StackPanel { Spacing = 14 }; panel.Children.Add(rows); panel.Children.Add(error);
        var form = new ContentDialog { Title = "Subscription links", Content = new ScrollViewer { Content = panel, MaxHeight = 460 }, CloseButtonText = "Done" };
        async Task RowAsync(CalendarFolder calendar, StackPanel row, string? shownUrl = null)
        {
            row.Children.Clear();
            var name = calendar.Name ?? "Calendar";
            row.Children.Add(Heading(name, 3));
            var feed = await model.FeedAsync(target, calendar.Id, lifetime);
            if (feed is null) { row.Children.Add(Text(model.State.Error ?? "The link state could not be read.")); return; }
            if (shownUrl is not null)
            {
                var box = new TextBox { Text = shownUrl, IsReadOnly = true, Header = "Subscription link" }; AutomationProperties.SetName(box, "Subscription link for " + name);
                row.Children.Add(box);
                row.Children.Add(Text("This link is shown only now. Paste it into the other app. Copies in other calendar apps are not recalled when you reset or turn off the link; they just stop updating."));
            }
            if (feed.Enabled)
            {
                row.Children.Add(Text(feed.Projection == "busy" ? "On — shows only busy times." : "On — shows your own titles and locations; private events and invitations as busy."));
                var buttons = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
                var reset = Button("Reset link", "Reset link for " + name); var off = Button("Turn off", "Turn off link for " + name);
                reset.Click += async (_, _) => { if (await model.SetFeedAsync(target, calendar.Id, true, feed.Projection, lifetime) is { } made) await RowAsync(calendar, row, made.Url); else error.Text = model.State.Error ?? ""; };
                off.Click += async (_, _) => { if (await model.SetFeedAsync(target, calendar.Id, false, null, lifetime) is not null) await RowAsync(calendar, row); else error.Text = model.State.Error ?? ""; };
                buttons.Children.Add(reset); buttons.Children.Add(off); row.Children.Add(buttons);
            }
            else
            {
                var projection = Choice("What the link shows for " + name, ["Busy times only", "My titles and locations"]);
                var on = Button("Turn on", "Turn on link for " + name);
                on.Click += async (_, _) =>
                {
                    var chosen = projection.SelectedIndex == 1 ? "personal_redacted" : "busy";
                    if (await model.SetFeedAsync(target, calendar.Id, true, chosen, lifetime) is { } made) await RowAsync(calendar, row, made.Url);
                    else error.Text = model.State.Error ?? "";
                };
                row.Children.Add(projection); row.Children.Add(on);
            }
        }
        foreach (var calendar in model.State.Folders)
        {
            var row = new StackPanel { Spacing = 6 }; rows.Children.Add(row);
            await RowAsync(calendar, row);
            if (!target.IsCurrent || disposed) return;
        }
        await Show(target, form);
    }

    // Import: the file is previewed by the postbox (nothing in it is fetched, nobody is invited) and only the ticked
    // entries are created.
    private async Task Import(CalendarActionTarget target)
    {
        byte[] bytes;
#if UI_TESTS
        if (Environment.GetEnvironmentVariable("PIGEONPOST_UI_CALENDAR_IMPORT_FILE") is { } fixture) bytes = await File.ReadAllBytesAsync(fixture);
        else
#endif
        {
            var picker = new Windows.Storage.Pickers.FileOpenPicker(); picker.FileTypeFilter.Add(".ics");
            WinRT.Interop.InitializeWithWindow.Initialize(picker, window);
            if (await picker.PickSingleFileAsync() is not { } file || !target.IsCurrent || disposed) return;
            if ((await file.GetBasicPropertiesAsync()).Size > CalendarSharing.MaxImportBytes) { await Problem(target, "That file is larger than 512 KB."); return; }
            var buffer = await Windows.Storage.FileIO.ReadBufferAsync(file);
            bytes = new byte[buffer.Length]; using (var reader = Windows.Storage.Streams.DataReader.FromBuffer(buffer)) reader.ReadBytes(bytes);
        }
        var folders = model.State.Folders;
        var into = Choice("Import into", folders.Count == 0 ? ["Default calendar"] : folders.Select(f => f.Name ?? "Calendar").ToArray());
        string? CalendarId() => folders.Count == 0 ? null : folders[Math.Max(0, into.SelectedIndex)].Id;
        var preview = await model.PreviewImportAsync(target, CalendarId(), bytes, lifetime);
        if (!target.IsCurrent || disposed) return;
        if (preview is null) { await Problem(target, model.State.Error ?? "This file could not be read."); return; }
        var fields = new StackPanel { Spacing = 10, MinWidth = 320 };
        fields.Children.Add(Text("Add events from this file. Nothing in it is fetched from the internet, and no invitations are sent."));
        fields.Children.Add(into);
        foreach (var warning in preview.Warnings) fields.Children.Add(Text(warning, 12));
        var picks = new List<(CheckBox Box, CalendarImportEntry Entry)>();
        foreach (var entry in preview.Events)
        {
            var box = new CheckBox { Content = entry.Label, IsChecked = entry.Importable, IsEnabled = entry.Importable };
            AutomationProperties.SetName(box, "Import " + entry.Label); fields.Children.Add(box); picks.Add((box, entry));
            foreach (var warning in entry.Warnings) fields.Children.Add(Text(warning, 12));
        }
        if (preview.Events.Count == 0) fields.Children.Add(Text("This file has no events to import."));
        var error = Text(""); fields.Children.Add(error);
        var form = Form("Import events", fields, preview.Events.Count == 1 ? "Import 1 event" : $"Import {preview.Events.Count} events");
        form.IsPrimaryButtonEnabled = preview.Events.Any(e => e.Importable);
        form.PrimaryButtonClick += async (sender, args) =>
        {
            args.Cancel = true; var deferred = args.GetDeferral(); form.IsPrimaryButtonEnabled = false; EnableFormFields(form, false);
            try
            {
                var select = picks.Where(p => p.Box.IsChecked == true).Select(p => p.Entry.Index).ToArray();
                if (select.Length == 0) { error.Text = "Choose at least one event."; return; }
                var created = await model.ConfirmImportAsync(target, preview.DraftId, select, CalendarId(), lifetime);
                if (created is { } n) { form.Hide(); scrollToHours = true; }
                else error.Text = model.State.Error ?? "The import was not confirmed. Check your calendar before trying again.";
            }
            catch (OperationCanceledException) { }
            finally { EnableFormFields(form, true); form.IsPrimaryButtonEnabled = target.IsCurrent; deferred.Complete(); }
        };
        await Show(target, form);
    }
}
