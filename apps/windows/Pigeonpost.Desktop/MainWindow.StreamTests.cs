#if UI_TESTS
using System.Text.Json;
using Pigeonpost.Core;
using Pigeonpost.Core.Groups;

namespace Pigeonpost.Desktop;

// UI-test build only (never in Store packages): drives the production stream path against the
// local stream harness (call-harness/streams.mjs). PIGEONPOST_CALLS_STREAM is
// {"action":"watch"|"golive","group":"grp_…","name":"…","stream":"str_…","title":"…"}; the stage
// is read from the harness's loopback postbox (PIGEONPOST_CALLS_POSTBOX) every two seconds.
public sealed partial class MainWindow
{
    private bool streamFixtureStarted;
    private async Task RunStreamFixtureAsync(CallPageTicket page)
    {
        if (streamFixtureStarted || CallsDebug("STREAM") is not { Length: <= 4096 } text) return;
        streamFixtureStarted = true;
        try
        {
            if (!Uri.TryCreate(CallsDebug("POSTBOX"), UriKind.Absolute, out var endpoint) || !endpoint.IsLoopback || endpoint.Scheme != "http"
                || endpoint.AbsolutePath != "/" || endpoint.UserInfo.Length > 0 || endpoint.Query.Length > 0)
                throw new InvalidOperationException("The stream fixture needs its loopback postbox.");
            using var configuration = JsonDocument.Parse(text); var fields = configuration.RootElement;
            var mailbox = page.Actor.Mailbox.Address;
            var client = new HttpClient(new HttpClientHandler { AllowAutoRedirect = false });
            var api = new GroupApi(new PostboxClient(client, CallTokenProvider(), endpoint), mailbox);
            streamStatusSource = async id => { try { var s = await api.StreamAsync(id, lifetime.Token); return s.IsLive ? s : null; } catch (Exception) { return null; } };
            await Task.Delay(1000, lifetime.Token);
            var group = fields.GetProperty("group").GetString()!; var name = fields.GetProperty("name").GetString()!;
            if (fields.GetProperty("action").GetString() == "golive")
                await StartStreamAsync(group, name, null, (fields.GetProperty("title").GetString()!, true, false), mailbox);
            else
                await StartStreamAsync(group, name, fields.GetProperty("stream").GetString()!, null, mailbox);
            while (!lifetime.IsCancellationRequested && CallPageCurrent(page))
            {
                await Task.Delay(2000, lifetime.Token);
                await ForwardStageAsync();
            }
        }
        catch (OperationCanceledException) { }
        catch (Exception error) { DesktopLog.Write("Stream fixture stopped: " + error.GetType().Name); }
    }
}
#endif
