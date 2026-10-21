#if UI_TESTS
using System.Net.Http;
using Pigeonpost.Core;
using Pigeonpost.Core.Preview;

namespace Pigeonpost.Desktop;

// UI-test build only: the stand-in postbox behind the Phase 1b and groups screens, with sample
// calls, filtered mail and a picture, for the preview mailboxes.
internal static class PreviewSideData
{
    public static void Seed(PreviewPostbox server)
    {
        var main = server.AddMailbox(PreviewInboxService.MainSeed, "/preview/main");
        server.AddMailbox(PreviewInboxService.TeamSeed, "/preview/team");
        const long at = 1789113600;
        server.AddCall(main, new CallLogEntry { Id = "call-missed", Direction = "incoming", Outcome = "missed", Kind = "video", Peer = "/k/design", PeerHandle = "/preview/design", StartedAt = at - 400 });
        server.AddCall(main, new CallLogEntry { Id = "call-out", Direction = "outgoing", Outcome = "answered", Kind = "audio", Peer = "/k/eng", PeerHandle = "/preview/engineering", StartedAt = at - 3000, AnsweredAt = at - 2995, EndedAt = at - 2800 });
        server.AddCall(main, new CallLogEntry { Id = "call-filtered", Direction = "incoming", Outcome = "filtered", Kind = "audio", Peer = "/k/unknowncaller", StartedAt = at - 5000 });
        server.AddFiltered(main, new InboxMessage { MessageId = "filtered-1", Body = "Limited offer just for you", From = "/k/promoter", SenderHandle = "/promo/deals", ReceivedAt = at - 600, Filtered = true });
    }
}

// Routes the UI-test build's requests: file upload and send to the attachment fixture, everything
// else to the stand-in postbox.
internal sealed class PreviewRouter(HttpMessageHandler attachments, HttpMessageHandler postbox) : HttpMessageHandler
{
    private readonly HttpMessageInvoker files = new(attachments);
    private readonly HttpMessageInvoker rest = new(postbox);
    protected override Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken token)
    {
        var path = request.RequestUri!.AbsolutePath;
        return path.StartsWith("/v1/attachments", StringComparison.Ordinal) || path == "/v1/send"
            ? files.SendAsync(request, token) : rest.SendAsync(request, token);
    }
}
#endif
