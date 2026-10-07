#if UI_TESTS
using System.Net;
using System.Net.Http;
using System.Text.Json;
using Pigeonpost.Core;

namespace Pigeonpost.Desktop;

// Compiled only into the disposable UI-test build, never Store packages.
internal sealed class AttachmentFixtureTokens : IAccessTokenProvider
{
    public Task<string> GetTokenAsync(CancellationToken token) => Task.FromResult("fixture-only");
    public Task<string> RefreshTokenAsync(CancellationToken token) => Task.FromResult("fixture-only");
}

internal sealed class AttachmentFixtureHandler(PreviewInboxService preview) : HttpMessageHandler
{
    private readonly Dictionary<string, (MessageAttachment File, byte[] Data, string Owner)> uploaded = [];
    private readonly Dictionary<string, HashSet<string>> readers = [];
    protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken token)
    {
        if (request.Method == HttpMethod.Get && request.RequestUri!.AbsolutePath.StartsWith("/v1/attachments/"))
        {
            var id = request.RequestUri.Segments.Last();
            var identity = request.Headers.GetValues("x-pigeonpost-identity").Single();
            if (!readers.TryGetValue(id, out var permitted) || !permitted.Contains(identity)) throw new InvalidOperationException("Download used the wrong mailbox.");
            return new(HttpStatusCode.OK) { Content = new ByteArrayContent(uploaded[id].Data) };
        }
        if (request.Method != HttpMethod.Post) throw new InvalidOperationException("Unexpected fixture request.");
        if (request.RequestUri!.AbsolutePath == "/v1/attachments")
        {
            var data = await request.Content!.ReadAsByteArrayAsync(token);
            var filename = request.Headers.GetValues("x-pigeonpost-filename").Single();
            var file = Path.Combine(Path.GetDirectoryName(Environment.GetEnvironmentVariable("PIGEONPOST_UI_ATTACHMENT_FILE"))!, filename);
            if (filename.StartsWith("image-") && filename.EndsWith(".png"))
            {
                if (data.Length < 24 || !data.AsSpan(0, 8).SequenceEqual(new byte[] { 137, 80, 78, 71, 13, 10, 26, 10 })
                    || System.Buffers.Binary.BinaryPrimitives.ReadUInt32BigEndian(data.AsSpan(16, 4)) != 2
                    || System.Buffers.Binary.BinaryPrimitives.ReadUInt32BigEndian(data.AsSpan(20, 4)) != 2)
                    throw new InvalidOperationException("Clipboard image was not encoded as the expected 2x2 PNG.");
                await File.WriteAllBytesAsync(file, data, token);
            }
            else
            {
                var expectedData = await File.ReadAllBytesAsync(file, token);
                if (!data.SequenceEqual(expectedData)) throw new InvalidOperationException("Windows file bytes changed.");
            }
            var owner = request.Headers.GetValues("x-pigeonpost-identity").Single();
            var attachment = new MessageAttachment("fixture-file-" + uploaded.Count, filename, request.Content.Headers.ContentType?.MediaType ?? "application/octet-stream", data.Length);
            uploaded.Add(attachment.Id, (attachment, data, owner));
            return Json(attachment);
        }
        if (request.RequestUri.AbsolutePath != "/v1/send") throw new InvalidOperationException("Unexpected fixture request.");
        using var json = JsonDocument.Parse(await request.Content!.ReadAsStringAsync(token));
        var body = json.RootElement;
        var ownerAddress = body.GetProperty("from").GetString()!;
        var ids = body.GetProperty("attachments").EnumerateArray().Select(v => v.GetString()!).ToArray();
        if (ids.Length == 0 || ids.Any(id => uploaded[id].Owner != ownerAddress)) throw new InvalidOperationException("File changed mailbox or attachment.");
        var caption = body.GetProperty("body").GetString()!;
        if (caption != "" && RequestEnvelope.DisplayText(caption) != "Optional file caption") throw new InvalidOperationException("Unexpected file caption.");
        var peer = body.GetProperty("to").GetString()!;
        var receipt = await preview.SendFilesAsync(ownerAddress, peer, caption,
            body.TryGetProperty("thread_id", out var thread) ? thread.GetString() : null, ids.Select(id => uploaded[id].File).ToArray(), token);
        var mailboxes = await preview.GetMailboxesAsync(token);
        foreach (var id in ids)
        {
            readers[id] = [ownerAddress];
            if (mailboxes.FirstOrDefault(m => m.Key == peer || m.Address == peer) is { } recipient) readers[id].Add(recipient.Address);
        }
        return Json(receipt);
    }
    private static HttpResponseMessage Json<T>(T value) => new(HttpStatusCode.Created)
    {
        Content = new StringContent(JsonSerializer.Serialize(value, PostboxJson.Options), System.Text.Encoding.UTF8, "application/json")
    };
}
#endif
