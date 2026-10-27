#if UI_TESTS
using System.Text.Json;
using Pigeonpost.Core;
using Pigeonpost.Core.Calendar;

namespace Pigeonpost.Desktop;

public sealed partial class MainWindow
{
    // One explicit synthetic login shared by the preview, token adapter and call host. This
    // code is absent from Store packages; the production account checks are used unchanged.
    private readonly AuthorizationLease? scheduledFixtureAuthorization = Environment.GetEnvironmentVariable("PIGEONPOST_CALLS_SCHEDULED") is null ? null : new();
    private bool scheduledFixtureStarted;
    private async Task RunScheduledFixtureAsync(CallPageTicket page)
    {
        if (scheduledFixtureStarted || CallsDebug("SCHEDULED") is not { Length: <= 4096 } text) return;
        scheduledFixtureStarted = true;
        try
        {
            if (scheduledFixtureAuthorization is null || !CallPageCurrent(page)
                || !Uri.TryCreate(CallsDebug("POSTBOX"), UriKind.Absolute, out var endpoint) || !endpoint.IsLoopback || endpoint.Scheme != "http"
                || endpoint.AbsolutePath != "/" || endpoint.UserInfo.Length > 0 || endpoint.Query.Length > 0 || endpoint.Fragment.Length > 0)
                throw new InvalidOperationException("Scheduled fixture requires its loopback postbox.");
            using var configuration = JsonDocument.Parse(text); var fields = configuration.RootElement;
            var key = new CalendarEventKey(fields.GetProperty("event_id").GetString()!, fields.GetProperty("occurrence_id").GetString());
            using var client = new HttpClient(new HttpClientHandler { AllowAutoRedirect = false });
            using var api = new PostboxClient(client, CallTokenProvider(), endpoint);
            using var model = new CalendarStore(api, api);
            var mailbox = page.Actor.Mailbox.Address; await model.ActivateAsync(mailbox, await api.GetCapabilitiesAsync(lifetime.Token), lifetime.Token);
            if (!CallPageCurrent(page)) return;
            using var target = model.CaptureView()?.BeginAction() ?? throw new InvalidOperationException("Scheduled fixture has no calendar view.");
            using var request = await model.PrepareScheduledJoinAsync(target, key, fields.GetProperty("code").GetString()!, fields.GetProperty("video").GetBoolean(), model.State.ContextId, lifetime.Token)
                ?? throw new InvalidOperationException("Scheduled fixture cannot prepare member admission.");
            await JoinPreparedMeetingAsync(request);
        }
        catch (Exception error) { DesktopLog.Write("Scheduled fixture stopped: " + error.GetType().Name); }
    }
}
#endif
