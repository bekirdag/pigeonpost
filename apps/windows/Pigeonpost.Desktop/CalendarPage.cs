using System.ComponentModel;
using System.Globalization;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Controls;
using Pigeonpost.Core.Calendar;

namespace Pigeonpost.Desktop;

internal delegate Task CalendarMeetingAction(CalendarActionTarget target, CalendarEventKey key, string code, bool video, Guid context);

// Calendar content is service-readable. Every control captures a rendered inbox view; each
// gesture then gets its own revocable target, including while a form or confirmation is open.
internal sealed partial class CalendarPage : UserControl, IDisposable
{
    private readonly CalendarStore model;
    private readonly nint window;
    private readonly CancellationToken lifetime;
    private readonly Func<bool> dialogBusy;
    private readonly Action<bool> setDialogBusy;
    private readonly CalendarMeetingAction joinMeeting;
    private readonly StackPanel contents = new() { Spacing = 18, Padding = new Thickness(24, 8, 24, 24) };
    private readonly ScrollViewer scroll;
    private CalendarContextView? rendered;
    private ContentDialog? dialog;
    private Guid context;
    private bool disposed;
    private bool filtersExpanded;
    private int renderQueued;
    private int renderVersion;

    internal CalendarPage(CalendarStore model, nint window, CancellationToken lifetime, Func<bool> dialogBusy, Action<bool> setDialogBusy, CalendarMeetingAction joinMeeting)
    {
        this.model = model; this.window = window; this.lifetime = lifetime; this.dialogBusy = dialogBusy; this.setDialogBusy = setDialogBusy;
        this.joinMeeting = joinMeeting;
        Content = scroll = new ScrollViewer { Content = contents, HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled };
        Loaded += (_, _) => Render();
        Unloaded += (_, _) => Dispose();
        model.PropertyChanged += StateChanged;
    }
    private void StateChanged(object? sender, PropertyChangedEventArgs args)
    {
        if (args.PropertyName != nameof(CalendarStore.State) || Interlocked.Exchange(ref renderQueued, 1) != 0) return;
        DispatcherQueue.TryEnqueue(() => { Interlocked.Exchange(ref renderQueued, 0); Render(); });
    }
    public void Dispose()
    {
        if (disposed) return; disposed = true; model.PropertyChanged -= StateChanged;
        rendered?.Retire(); rendered = null; dialog?.Hide(); contents.Children.Clear();
    }
    private static TextBlock Text(string text, double size = 13) => new() { Text = text, FontSize = size, TextWrapping = TextWrapping.Wrap };
    private static TextBlock Heading(string text, int level = 2)
    {
        var block = Text(text, level == 2 ? 17 : 14); block.FontWeight = FontWeights.SemiBold;
        AutomationProperties.SetHeadingLevel(block, level == 2 ? AutomationHeadingLevel.Level2 : AutomationHeadingLevel.Level3); return block;
    }
    private static Button Button(string label, string? accessible = null)
    {
        var button = new Button { Content = label, MinHeight = 32, Padding = new Thickness(10, 5, 10, 5), CornerRadius = new CornerRadius(7) };
        AutomationProperties.SetName(button, accessible ?? label); return button;
    }
    private static TextBox Field(string label, string value)
    {
        var box = new TextBox { Header = label, Text = value, HorizontalAlignment = HorizontalAlignment.Stretch };
        AutomationProperties.SetName(box, label); return box;
    }
    private static ComboBox Choice(string label, IReadOnlyList<string> values, int selected = 0)
    {
        var box = new ComboBox { Header = label, HorizontalAlignment = HorizontalAlignment.Stretch };
        foreach (var value in values) box.Items.Add(value); box.SelectedIndex = selected; AutomationProperties.SetName(box, label); return box;
    }
    private Button Action(string label, CalendarContextView view, Func<CalendarActionTarget, Task> run, bool enabled = true, string? accessible = null)
    {
        var button = Button(label, accessible); button.IsEnabled = enabled;
        button.Click += async (_, _) =>
        {
            using var target = view.BeginAction(); if (target is null || disposed) return;
            button.IsEnabled = false;
            try { await run(target); }
            catch (OperationCanceledException) { }
            catch (Exception) { if (target.IsCurrent && !disposed) await Problem(target, "Could not complete this calendar action. Your form has not been sent again."); }
            finally { if (!disposed && view.IsCurrent) button.IsEnabled = enabled; }
        };
        return button;
    }
    private static string TimeLabel(CalendarOccurrence row, string zone)
    {
        if (row.AllDay == true) return "All day";
        if (row.StartsAt is not { } start || row.EndsAt is not { } end) return "Time unavailable";
        try
        {
            string At(long at) => NodaTime.Instant.FromUnixTimeSeconds(at).InZone(CalendarTime.Zone(zone)).LocalDateTime
                .ToDateTimeUnspecified().ToString("HH:mm", CultureInfo.CurrentCulture);
            return At(start) + "–" + At(end);
        }
        catch (ArgumentException) { return "Time unavailable"; }
    }
    private void Render()
    {
        if (disposed) return;
        var version = ++renderVersion; var offset = scroll.VerticalOffset; var state = model.State;
        if (context != state.ContextId || !state.Enabled)
        {
            rendered?.Retire(); rendered = null; dialog?.Hide(); context = state.ContextId;
        }
        // Background loading/rendering does not change the inbox authority. A gesture queued
        // from the same inbox still checks fresh access; only leaving that context retires it.
        var next = rendered ??= model.CaptureView();
        contents.Children.Clear();
        if (!state.Enabled || next is null) { contents.Children.Add(Text(state.Error ?? "The calendar is unavailable for this inbox.")); return; }
        var view = next;
        var toolbar = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 6 };
        toolbar.Children.Add(Action("‹", view, async target => { if (model.SelectDay(target, new(state.SelectedDay.Date.AddMonths(-1)))) await model.ReloadAsync(target, lifetime); }, accessible: "Previous month"));
        toolbar.Children.Add(Heading(state.SelectedDay.Date.ToString("MMMM yyyy", CultureInfo.CurrentCulture)));
        toolbar.Children.Add(Action("›", view, async target => { if (model.SelectDay(target, new(state.SelectedDay.Date.AddMonths(1)))) await model.ReloadAsync(target, lifetime); }, accessible: "Next month"));
        toolbar.Children.Add(Action("Today", view, async target => { if (model.Today(target)) await model.ReloadAsync(target, lifetime); }));
        toolbar.Children.Add(Action("Refresh", view, target => model.ReloadAsync(target, lifetime), !state.Loading));
        contents.Children.Add(toolbar);
        var actions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        actions.Children.Add(Action("New event", view, target => Edit(target), state.CanCreate && !state.Pending.Any(p => p.Kind == CalendarAttemptKind.Event)));
        actions.Children.Add(Action("New calendar", view, NewCalendar, state.CanCreate && !state.Pending.Any(p => p.Kind == CalendarAttemptKind.Calendar)));
        actions.Children.Add(Action("Calendar settings", view, Settings, !state.Busy)); contents.Children.Add(actions);
        if (state.Error is { Length: > 0 } error) contents.Children.Add(Text(error));
        if (state.Loading) contents.Children.Add(new ProgressBar { IsIndeterminate = true, Height = 3 });
        if (state.Pending.Count > 0)
        {
            contents.Children.Add(Heading("Pending requests"));
            foreach (var pending in state.Pending)
                contents.Children.Add(Action(pending.Kind == CalendarAttemptKind.Event ? "Review pending event" : "Review pending calendar", view, target => Recovery(target, pending), !state.Busy));
        }
        var grid = new Grid { RowSpacing = 4, ColumnSpacing = 4 };
        for (var i = 0; i < 7; i++) { grid.ColumnDefinitions.Add(new() { Width = new(1, GridUnitType.Star) }); grid.RowDefinitions.Add(new() { Height = GridLength.Auto }); }
        var days = new CalendarMonthWindow(state.SelectedDay, state.WeekStart).Days;
        for (var column = 0; column < 7; column++)
        {
            var label = Text(CultureInfo.CurrentCulture.DateTimeFormat.AbbreviatedDayNames[(column + state.WeekStart) % 7], 12);
            label.HorizontalAlignment = HorizontalAlignment.Center; Grid.SetColumn(label, column); grid.Children.Add(label);
        }
        for (var index = 0; index < days.Count; index++)
        {
            var day = days[index]; var count = state.Occurrences.Count(o => o.OccursOn(day, CalendarTime.Zone(state.DisplayZone)));
            var button = Action(day.Date.Day + (count == 0 ? "" : "\n• " + count), view,
                async target => { if (model.SelectDay(target, day)) await model.ReloadAsync(target, lifetime); }, accessible: $"{day.Text}, {count} events");
            button.MinHeight = 54; button.HorizontalAlignment = HorizontalAlignment.Stretch;
            button.Opacity = day.Date.Month == state.SelectedDay.Date.Month ? 1 : .55;
            if (day == state.SelectedDay) button.Style = (Style)Application.Current.Resources["AccentButtonStyle"];
            Grid.SetRow(button, index / 7 + 1); Grid.SetColumn(button, index % 7); grid.Children.Add(button);
        }
        contents.Children.Add(grid);
        contents.Children.Add(Text("Times shown in " + state.DisplayZone, 12));
        var filters = new StackPanel { Spacing = 7 };
        var filterActions = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        filterActions.Children.Add(Action("All calendars", view, async target => { if (model.SelectCalendars(target, null)) await model.ReloadAsync(target, lifetime); }));
        filterActions.Children.Add(Action("No calendars", view, async target => { if (model.SelectCalendars(target, [])) await model.ReloadAsync(target, lifetime); })); filters.Children.Add(filterActions);
        foreach (var calendar in state.Folders)
        {
            var check = new CheckBox { Content = calendar.Name ?? "Calendar", IsChecked = state.SelectedCalendars?.Contains(calendar.Id) ?? true };
            AutomationProperties.SetName(check, "Show calendar " + (calendar.Name ?? "Calendar"));
            check.Click += async (_, _) =>
            {
                using var target = view.BeginAction(); if (target is null) return;
                var selection = state.SelectedCalendars?.ToHashSet(StringComparer.Ordinal) ?? state.Folders.Select(f => f.Id).ToHashSet(StringComparer.Ordinal);
                if (check.IsChecked == true) selection.Add(calendar.Id); else selection.Remove(calendar.Id);
                if (model.SelectCalendars(target, selection)) await model.ReloadAsync(target, lifetime);
            };
            filters.Children.Add(check);
        }
        var filterExpander = new Expander { Header = "Calendars", Content = filters, IsExpanded = filtersExpanded, HorizontalAlignment = HorizontalAlignment.Stretch };
        filterExpander.Expanding += (_, _) => filtersExpanded = true; filterExpander.Collapsed += (_, _) => filtersExpanded = false;
        contents.Children.Add(filterExpander);
        contents.Children.Add(Heading(state.SelectedDay.Date.ToString("dddd, d MMMM", CultureInfo.CurrentCulture)));
        if (state.Agenda.Count == 0) contents.Children.Add(Text(state.Folders.Count == 0 ? "Create an event or calendar to get started." : "No events on this day."));
        foreach (var row in state.Agenda)
        {
            var button = Action(row.Title ?? "Untitled event", view, target => Detail(target, row.Key), accessible: $"Open event {row.Title}, {TimeLabel(row, state.DisplayZone)}");
            button.Content = new StackPanel { Spacing = 4, Children = { Text(row.Title ?? "Untitled event", 15), Text(TimeLabel(row, state.DisplayZone) + (row.Meeting is { Enabled: true } or { Code.Length: > 0 } ? " · Pigeonpost meeting" : "") + (row.Status == "cancelled" ? " · Cancelled" : ""), 12) } };
            button.HorizontalAlignment = HorizontalAlignment.Stretch; button.HorizontalContentAlignment = HorizontalAlignment.Left; contents.Children.Add(button);
        }
        void Invitations(string title, IReadOnlyList<CalendarInvitation> invitations)
        {
            if (invitations.Count == 0) return; contents.Children.Add(Heading(title));
            foreach (var item in invitations) contents.Children.Add(Action(item.Title ?? "Untitled invitation", view, target => Detail(target, item.Key), accessible: "Open invitation " + item.Title));
        }
        Invitations("Invitations", state.Invitations); Invitations("Filtered invitations", state.FilteredInvitations);
        DispatcherQueue.TryEnqueue(Microsoft.UI.Dispatching.DispatcherQueuePriority.Low, () => { if (!disposed && version == renderVersion && ReferenceEquals(rendered, view)) scroll.ChangeView(null, offset, null, true); });
    }
    private async Task<ContentDialogResult> Show(CalendarActionTarget target, ContentDialog next)
    {
        if (disposed || !target.IsCurrent || XamlRoot is null || dialog is not null || dialogBusy()) return ContentDialogResult.None;
        next.XamlRoot = XamlRoot; next.RequestedTheme = ActualTheme; dialog = next; setDialogBusy(true); scroll.IsEnabled = false;
        try { var result = await next.ShowAsync(); return !disposed && target.IsCurrent ? result : ContentDialogResult.None; }
        finally { if (ReferenceEquals(dialog, next)) { dialog = null; setDialogBusy(false); if (!disposed) scroll.IsEnabled = true; } }
    }
    private Task Problem(CalendarActionTarget target, string message) => Show(target, new ContentDialog { Title = "Calendar", Content = Text(message), CloseButtonText = "Done" });
}
