#if UI_TESTS
using System.Net.Http;
using Pigeonpost.Core;
using Pigeonpost.Core.Preview;

namespace Pigeonpost.Desktop;

// UI-test build only: the stand-in postbox behind the Phase 1b and groups screens, with sample
// calls, filtered mail and a picture, for the preview mailboxes.
internal static class PreviewSideData
{
    public static void SeedRooms(PreviewPostbox server) => PreviewRooms.Seed(server);
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

internal static class PreviewRooms
{
    public const string OrganizationId = "org_preview";
    // The team inbox is the person's organization member inbox; the main inbox is personal.
    public static void Seed(PreviewPostbox server)
    {
        const string team = "/k/rywv63mb6h0hhrpfjewrrttspw"; // PreviewInboxService.TeamSeed's canonical inbox.
        var owner = server.AddMailbox(PreviewPostbox.Seed(0xB3), "/example/owner");
        server.AddOrganization(OrganizationId, "Example Organization", [team, owner], [("/k/cz6900v2h90vnwefj7g7ezvbh4", "/example/agent")]);
        server.SeedOrganizationRoom(OrganizationId, owner, "Design studio", "public", "Product design reviews");
        server.SeedOrganizationRoom(OrganizationId, owner, "Leadership", "private", "Invitation only");
    }
}

// Synthetic committed-response loss for organization room creation, with attempts recorded across
// app restarts so the UI test can prove one exact request is retried only by an explicit action.
internal sealed class RoomLossHandler : DelegatingHandler
{
    private readonly string statePath;
    private int losses;
    private readonly List<object> attempts = [];
    public RoomLossHandler(HttpMessageHandler inner, string statePath) : base(inner)
    {
        this.statePath = statePath;
        losses = int.TryParse(Environment.GetEnvironmentVariable("PIGEONPOST_UI_ROOMS_LOSSES"), out var count) ? count : 0;
        if (File.Exists(statePath))
        {
            using var saved = System.Text.Json.JsonDocument.Parse(File.ReadAllText(statePath));
            losses = saved.RootElement.GetProperty("losses").GetInt32();
            foreach (var row in saved.RootElement.GetProperty("attempts").EnumerateArray())
                attempts.Add(new { sha = row.GetProperty("sha").GetString(), key = row.GetProperty("key").GetString() });
        }
        Save();
    }
    protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken token)
    {
        if (request.Method != HttpMethod.Post || request.RequestUri!.AbsolutePath != "/v1/groups" || request.Content is null)
            return await base.SendAsync(request, token);
        var body = await request.Content.ReadAsStringAsync(token);
        using var parsed = System.Text.Json.JsonDocument.Parse(body);
        if (!parsed.RootElement.TryGetProperty("org_id", out _)) return await base.SendAsync(request, token);
        var sha = Convert.ToHexString(System.Security.Cryptography.SHA256.HashData(System.Text.Encoding.UTF8.GetBytes(body)));
        attempts.Add(new { sha, key = parsed.RootElement.GetProperty("request_key").GetString() });
        var response = await base.SendAsync(request, token);
        if (losses > 0 && response.IsSuccessStatusCode) { losses--; Save(); response.Dispose(); throw new HttpRequestException("Synthetic committed room creation with a lost reply."); }
        Save(); return response;
    }
    private void Save()
    {
        var temporary = statePath + ".new";
        File.WriteAllText(temporary, System.Text.Json.JsonSerializer.Serialize(new { losses, attempts }));
        File.Move(temporary, statePath, overwrite: true);
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
