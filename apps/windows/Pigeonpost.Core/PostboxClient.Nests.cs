using System.Text.Json;
using Pigeonpost.Core.Nests;

namespace Pigeonpost.Core;

// Nests (docs/nests/phase-b.md) over the accepted-login transport. Account-level routes: no
// mailbox selector. Nothing here buys anything; purchases happen on the website.
public sealed partial class PostboxClient : INestApi
{
    private const int MaxNestReply = 1024 * 1024;
    // The Nests host allows 512 MiB of files; the archive adds parsed text and conversations.
    private const int MaxExport = 1024 * 1024 * 1024;

    private async Task<JsonElement> NestJsonAsync(HttpMethod method, string path, string? json, CancellationToken ct)
    {
        var owner = new RequestAuthorization(tokens);
        using var response = await RequestAsync(method, path, json, ct).ConfigureAwait(false);
        try
        {
            await response.Content.LoadIntoBufferAsync(MaxNestReply, ct).ConfigureAwait(false);
            var bytes = await response.Content.ReadAsByteArrayAsync(ct).ConfigureAwait(false);
            owner.Check(ct);
            if (bytes.Length == 0) return default;
            using var document = JsonDocument.Parse(bytes);
            return document.RootElement.Clone();
        }
        catch (Exception error) when (error is JsonException or HttpRequestException or InvalidOperationException)
        { owner.Check(ct); throw new NestWireException(); }
    }

    private static string NestPath(string nestId, string suffix = "") =>
        NestRules.ValidNest(nestId) ? "/v1/nests/" + nestId + suffix : throw new ArgumentException("Invalid Nest.", nameof(nestId));

    public async Task<IReadOnlyList<NestPackage>> NestPackagesAsync(CancellationToken ct) =>
        NestRules.Packages(await NestJsonAsync(HttpMethod.Get, "/v1/nest-packages", null, ct).ConfigureAwait(false));

    public async Task<NestSnapshot> NestSnapshotAsync(CancellationToken ct) =>
        NestRules.Snapshot(await NestJsonAsync(HttpMethod.Get, "/v1/nests", null, ct).ConfigureAwait(false));

    public async Task<NestRow> NestAsync(string nestId, CancellationToken ct) =>
        NestRules.Nest(await NestJsonAsync(HttpMethod.Get, NestPath(nestId), null, ct).ConfigureAwait(false));

    public async Task<NestSnapshot> RetryNestSetupAsync(string setupId, CancellationToken ct) =>
        NestRules.ValidSetup(setupId)
            ? NestRules.Snapshot(await NestJsonAsync(HttpMethod.Post, "/v1/nest-setups/" + setupId + "/retry", "{}", ct).ConfigureAwait(false))
            : throw new ArgumentException("Invalid setup.", nameof(setupId));

    public async Task<NestFiles> NestFilesAsync(string nestId, CancellationToken ct) =>
        NestRules.Files(await NestJsonAsync(HttpMethod.Get, NestPath(nestId, "/files"), null, ct).ConfigureAwait(false));

    public async Task UploadNestFileAsync(string nestId, string name, byte[] bytes, CancellationToken ct)
    {
        if (bytes.Length is 0 or > NestRules.MaxUpload) throw new PostboxException(413, "too_large");
        var header = NestRules.UploadHeader(name);
        using var response = await RequestAsync(HttpMethod.Post, NestPath(nestId, "/files"), null, ct, bytes, transfer: true,
            headers: new Dictionary<string, string> { ["x-file-name"] = header }).ConfigureAwait(false);
    }

    public async Task DeleteNestFileAsync(string nestId, string fileId, CancellationToken ct)
    {
        if (!NestRules.ValidFile(fileId)) throw new ArgumentException("Invalid file.", nameof(fileId));
        await NestJsonAsync(HttpMethod.Delete, NestPath(nestId, "/files/" + fileId), null, ct).ConfigureAwait(false);
    }

    public async Task<IReadOnlyList<NestApproval>> NestApprovalsAsync(string nestId, CancellationToken ct) =>
        NestRules.Approvals(await NestJsonAsync(HttpMethod.Get, NestPath(nestId, "/approvals"), null, ct).ConfigureAwait(false));

    public async Task DecideNestApprovalAsync(string nestId, NestApproval card, bool approve, CancellationToken ct) =>
        await NestJsonAsync(HttpMethod.Post, NestPath(nestId, "/approvals/" + card.Id + (approve ? "/approve" : "/reject")),
            NestRules.DecisionJson(card), ct).ConfigureAwait(false);

    public async Task<IReadOnlyList<string>> NestUsageAsync(string nestId, CancellationToken ct) =>
        NestRules.UsageLines(await NestJsonAsync(HttpMethod.Get, NestPath(nestId, "/usage"), null, ct).ConfigureAwait(false));

    public async Task PauseNestAsync(string nestId, bool resume, CancellationToken ct) =>
        await NestJsonAsync(HttpMethod.Post, NestPath(nestId, resume ? "/resume" : "/pause"), "{}", ct).ConfigureAwait(false);

    public async Task RenameNestAsync(string nestId, string label, CancellationToken ct) =>
        await NestJsonAsync(HttpMethod.Patch, NestPath(nestId), JsonSerializer.Serialize(new Dictionary<string, string> { ["label"] = label }), ct).ConfigureAwait(false);

    public async Task<byte[]> ExportNestAsync(string nestId, CancellationToken ct)
    {
        using var response = await RequestAsync(HttpMethod.Get, NestPath(nestId, "/export"), null, ct, transfer: true).ConfigureAwait(false);
        await response.Content.LoadIntoBufferAsync(MaxExport, ct).ConfigureAwait(false);
        var bytes = await response.Content.ReadAsByteArrayAsync(ct).ConfigureAwait(false);
        CheckResponseAuthorization(response, ct);
        return bytes;
    }

    public async Task DeleteNestAsync(string nestId, string label, string typed, CancellationToken ct)
    {
        if (!NestRules.DeleteConfirmed(label, typed)) throw new PostboxException(400, "confirm_required");
        await NestJsonAsync(HttpMethod.Delete, NestPath(nestId), JsonSerializer.Serialize(new Dictionary<string, string> { ["confirm"] = typed }), ct).ConfigureAwait(false);
    }

    public async Task<OrgNests> OrgNestsAsync(string orgId, CancellationToken ct) =>
        NestRules.OrgNestsOf(await NestJsonAsync(HttpMethod.Get, "/v1/orgs/" + Uri.EscapeDataString(orgId) + "/nests", null, ct).ConfigureAwait(false));

    public async Task CreateOrgNestAsync(string orgId, string handle, string label, CancellationToken ct) =>
        await NestJsonAsync(HttpMethod.Post, "/v1/orgs/" + Uri.EscapeDataString(orgId) + "/nests",
            JsonSerializer.Serialize(new Dictionary<string, string> { ["handle"] = handle, ["label"] = label }), ct).ConfigureAwait(false);
}
