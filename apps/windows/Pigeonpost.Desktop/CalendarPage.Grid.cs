using System.Globalization;
using Microsoft.UI;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media;
using Pigeonpost.Core.Calendar;

namespace Pigeonpost.Desktop;

// Audit P2-18: week and day time grids with a "now" line, beside the month view. Blocks open the same event detail
// as the agenda; overlapping events share their day column side by side.
internal sealed partial class CalendarPage
{
    private static CalendarView calendarView = CalendarView.Month;
    private bool scrollToHours = true;
    private double? hoursTop;
    private DispatcherTimer? nowTimer;
    private const double HourHeight = 48;

    private static string RangeTitle(CalendarState state)
    {
        var culture = CultureInfo.CurrentCulture;
        switch (calendarView)
        {
            case CalendarView.Day: return state.SelectedDay.Date.ToString("dddd, d MMMM yyyy", culture);
            case CalendarView.Week:
                var days = CalendarTimeGrid.Days(CalendarView.Week, state.SelectedDay, state.WeekStart);
                var first = days[0].Date; var last = days[^1].Date;
                return first.Month == last.Month ? $"{first.Day}–{last.ToString("d MMMM yyyy", culture)}"
                    : $"{first.ToString("d MMM", culture)} – {last.ToString("d MMM yyyy", culture)}";
            default: return state.SelectedDay.Date.ToString("MMMM yyyy", culture);
        }
    }

    private StackPanel ViewPicker()
    {
        var picker = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 4 };
        AutomationProperties.SetName(picker, "Calendar view");
        foreach (var (label, value) in new[] { ("Month", CalendarView.Month), ("Week", CalendarView.Week), ("Day", CalendarView.Day) })
        {
            var button = Button(label, label + " view");
            if (value == calendarView)
            {
                button.Style = (Style)Application.Current.Resources["AccentButtonStyle"];
                AutomationProperties.SetItemStatus(button, "Selected");
            }
            button.Click += (_, _) => { if (calendarView == value) return; calendarView = value; scrollToHours = true; Render(); };
            picker.Children.Add(button);
        }
        return picker;
    }

    private void RenderTimeGrid(CalendarContextView view, CalendarState state)
    {
        var zone = CalendarTime.Zone(state.DisplayZone);
        var days = CalendarTimeGrid.Days(calendarView, state.SelectedDay, state.WeekStart);
        var blocks = CalendarTimeGrid.Layout(state.Occurrences, days, zone);
        var now = CalendarTimeGrid.Now(NodaTime.SystemClock.Instance.GetCurrentInstant(), days, zone);
        var culture = CultureInfo.CurrentCulture;

        // Day headers: selecting one moves the day view and the agenda's day there.
        var header = new Grid { ColumnSpacing = 2 };
        header.ColumnDefinitions.Add(new() { Width = new GridLength(56) });
        for (var i = 0; i < days.Count; i++) header.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        for (var i = 0; i < days.Count; i++)
        {
            var day = days[i];
            var label = day.Date.ToString(calendarView == CalendarView.Day ? "dddd d" : "ddd d", culture);
            var button = Action(label, view, async target => { if (model.SelectDay(target, day)) await model.ReloadAsync(target, lifetime); }, accessible: $"{day.Text}, {blocks.Count(b => b.Day == i)} timed events");
            button.HorizontalAlignment = HorizontalAlignment.Stretch;
            if (day == state.SelectedDay && calendarView == CalendarView.Week) button.Style = (Style)Application.Current.Resources["AccentButtonStyle"];
            Grid.SetColumn(button, i + 1); header.Children.Add(button);
        }
        contents.Children.Add(header);

        // All-day events sit above the hours.
        var allDay = new Grid { ColumnSpacing = 2 };
        allDay.ColumnDefinitions.Add(new() { Width = new GridLength(56) });
        var anyAllDay = false;
        for (var i = 0; i < days.Count; i++)
        {
            allDay.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
            var stack = new StackPanel { Spacing = 2 };
            foreach (var row in CalendarTimeGrid.AllDay(state.Occurrences, days[i], zone))
            {
                anyAllDay = true;
                var chip = Action(row.Title ?? "Untitled event", view, target => Detail(target, row.Key), accessible: $"Open event {row.Title}, all day {days[i].Text}");
                chip.HorizontalAlignment = HorizontalAlignment.Stretch; chip.HorizontalContentAlignment = HorizontalAlignment.Left; chip.MinHeight = 26; chip.FontSize = 12;
                stack.Children.Add(chip);
            }
            Grid.SetColumn(stack, i + 1); allDay.Children.Add(stack);
        }
        if (anyAllDay) { var caption = Text("All day", 11); caption.VerticalAlignment = VerticalAlignment.Center; allDay.Children.Add(caption); contents.Children.Add(allDay); }

        // The hours.
        var hours = new Grid { ColumnSpacing = 2, Height = 24 * HourHeight };
        AutomationProperties.SetName(hours, calendarView == CalendarView.Day ? "Day time grid" : "Week time grid");
        hours.ColumnDefinitions.Add(new() { Width = new GridLength(56) });
        for (var i = 0; i < days.Count; i++) hours.ColumnDefinitions.Add(new() { Width = new GridLength(1, GridUnitType.Star) });
        var gutter = new Canvas();
        for (var hour = 0; hour < 24; hour++)
        {
            var mark = Text(new TimeOnly(hour, 0).ToString("HH:mm", culture), 11);
            mark.Opacity = .7; Canvas.SetTop(mark, hour * HourHeight - (hour == 0 ? 0 : 7)); gutter.Children.Add(mark);
        }
        AutomationProperties.SetAccessibilityView(gutter, Microsoft.UI.Xaml.Automation.Peers.AccessibilityView.Raw);
        hours.Children.Add(gutter);
        var line = (Brush)Application.Current.Resources["CardStrokeColorDefaultBrush"];
        var card = (Brush)Application.Current.Resources["AccentFillColorDefaultBrush"];
        var cardText = (Brush)Application.Current.Resources["TextOnAccentFillColorPrimaryBrush"];
        for (var i = 0; i < days.Count; i++)
        {
            var column = new Grid();
            for (var hour = 0; hour < 24; hour++)
            {
                column.RowDefinitions.Add(new() { Height = new GridLength(HourHeight) });
                var cell = new Border { BorderBrush = line, BorderThickness = new Thickness(1, hour == 0 ? 1 : 0, 0, 1) };
                Grid.SetRow(cell, hour); column.Children.Add(cell);
            }
            var canvas = new Canvas(); Grid.SetRowSpan(canvas, 24); column.Children.Add(canvas);
            var placed = new List<(FrameworkElement Element, CalendarGridBlock Block)>();
            foreach (var block in blocks.Where(b => b.Day == i))
            {
                var row = block.Row;
                var time = TimeLabel(row, state.DisplayZone);
                var button = Action(row.Title ?? "Untitled event", view, target => Detail(target, row.Key), accessible: $"Open event {row.Title}, {time}, {days[i].Text}");
                button.Background = card; button.Foreground = cardText; button.Padding = new Thickness(6, 2, 6, 2);
                button.VerticalContentAlignment = VerticalAlignment.Top; button.HorizontalContentAlignment = HorizontalAlignment.Left;
                button.Opacity = row.Status == "cancelled" ? .5 : 1;
                button.Content = new StackPanel { Children = { new TextBlock { Text = row.Title ?? "Untitled event", FontSize = 12, FontWeight = FontWeights.SemiBold, TextTrimming = TextTrimming.CharacterEllipsis },
                    new TextBlock { Text = time + (block.ContinuesAfter ? " →" : ""), FontSize = 11, TextTrimming = TextTrimming.CharacterEllipsis } } };
                button.MinHeight = 0; button.Height = Math.Max(18, (block.EndMinute - block.StartMinute) / 60 * HourHeight - 2);
                Canvas.SetTop(button, block.StartMinute / 60 * HourHeight + 1);
                canvas.Children.Add(button); placed.Add((button, block));
            }
            Border? nowLine = null;
            if (now is { } n && n.Day == i)
            {
                nowLine = new Border { Height = 2, Background = new SolidColorBrush(Colors.Red), IsHitTestVisible = false };
                Canvas.SetTop(nowLine, n.Minute / 60 * HourHeight - 1); Canvas.SetZIndex(nowLine, 10);
                canvas.Children.Add(nowLine);
                // The line itself has no accessible peer; this label says where "now" is for Narrator and the UI test.
                var label = new TextBlock { Text = "now", FontSize = 9, Foreground = new SolidColorBrush(Colors.Red), IsHitTestVisible = false };
                AutomationProperties.SetName(label, "Now, " + TimeOnly.FromTimeSpan(TimeSpan.FromMinutes(Math.Floor(n.Minute))).ToString("HH:mm", CultureInfo.InvariantCulture));
                Canvas.SetTop(label, Math.Max(0, n.Minute / 60 * HourHeight - 13)); Canvas.SetLeft(label, 2); Canvas.SetZIndex(label, 11);
                canvas.Children.Add(label);
            }
            canvas.SizeChanged += (_, e) =>
            {
                var width = Math.Max(0, e.NewSize.Width - 4);
                foreach (var (element, block) in placed)
                {
                    var laneWidth = width / Math.Max(1, block.Lanes);
                    element.Width = Math.Max(16, laneWidth - 2); Canvas.SetLeft(element, 2 + block.Lane * laneWidth);
                }
                if (nowLine is not null) nowLine.Width = e.NewSize.Width;
            };
            Grid.SetColumn(column, i + 1); hours.Children.Add(column);
        }
        contents.Children.Add(hours);
        contents.Children.Add(Text("Times shown in " + state.DisplayZone, 12));
        hoursTop = null;
        var first = CalendarTimeGrid.FirstMinute(blocks, now);
        hours.Loaded += (_, _) =>
        {
            try { hoursTop = hours.TransformToVisual(contents).TransformPoint(new Windows.Foundation.Point(0, 0)).Y + first / 60 * HourHeight; }
            catch (ArgumentException) { hoursTop = null; }
            if (scrollToHours && hoursTop is { } top) { scrollToHours = false; scroll.ChangeView(null, top, null, true); }
        };
        // Keep the now line moving while a week or day is shown.
        nowTimer ??= new DispatcherTimer { Interval = TimeSpan.FromMinutes(1) };
        nowTimer.Tick -= NowTick; nowTimer.Tick += NowTick;
        if (now is not null) nowTimer.Start(); else nowTimer.Stop();
    }
    private void NowTick(object? sender, object e) { if (disposed || calendarView == CalendarView.Month) { nowTimer?.Stop(); return; } Render(); }
}
