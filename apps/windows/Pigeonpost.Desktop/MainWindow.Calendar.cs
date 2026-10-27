using Microsoft.UI.Xaml;
using Pigeonpost.Core.Calendar;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow
{
    private CalendarStore calendarModel = null!;
    private CalendarPage? calendarPage;
#if UI_TESTS
    private NativeCalendarFixture? calendarUiFixture;
#endif
    private void CreateCalendarModel()
    {
#if UI_TESTS
        if (Environment.GetEnvironmentVariable("PIGEONPOST_UI_CALENDAR_STATE") is { } state)
        {
            calendarUiFixture = new(preview, state);
            var fixtureFolder = Environment.GetEnvironmentVariable("PIGEONPOST_UI_CALENDAR_FOLDER") ?? throw new InvalidOperationException("Calendar fixture folder missing.");
            calendarModel = new(calendarUiFixture, calendarUiFixture, new CalendarDeliveryCoordinator(calendarUiFixture, new FileCalendarAttemptStore(fixtureFolder, new DpapiProtector())));
            return;
        }
#endif
        var folder = Path.Combine(Environment.GetFolderPath(Environment.SpecialFolder.LocalApplicationData), "Pigeonpost", "calendar-attempts");
        calendarModel = new(postbox, postbox, new CalendarDeliveryCoordinator(postbox, new FileCalendarAttemptStore(folder, new DpapiProtector())));
    }
    private FrameworkElement BuildCalendarPage()
    {
        var frame = PageFrame("Calendar", out _, out var body);
        calendarPage = new(calendarModel, WinRT.Interop.WindowNative.GetWindowHandle(this), lifetime.Token, () => dialogOpen, value => dialogOpen = value);
        body.Children.Add(calendarPage); return frame;
    }
}
