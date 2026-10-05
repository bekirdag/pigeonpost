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
    private MessageAttachment? uploaded;
    private string? owner;
    private int sends;
    protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken token)
    {
        if (request.Method != HttpMethod.Post) throw new InvalidOperationException("Unexpected fixture request.");
        var file = Environment.GetEnvironmentVariable("PIGEONPOST_UI_ATTACHMENT_FILE")!;
        if (request.RequestUri!.AbsolutePath == "/v1/attachments")
        {
            var data = await request.Content!.ReadAsByteArrayAsync(token);
            var expectedData = await File.ReadAllBytesAsync(file, token);
            if (!data.SequenceEqual(expectedData)) throw new InvalidOperationException("Windows file bytes changed.");
            owner = request.Headers.GetValues("x-pigeonpost-identity").Single();
            var filename = request.Headers.GetValues("x-pigeonpost-filename").Single();
            if (filename != Path.GetFileName(file)) throw new InvalidOperationException("Wrong filename.");
            uploaded = new("fixture-file", filename, "text/plain", data.Length);
            return Json(uploaded);
        }
        if (request.RequestUri.AbsolutePath != "/v1/send" || uploaded is null) throw new InvalidOperationException("Send happened before upload.");
        using var json = JsonDocument.Parse(await request.Content!.ReadAsStringAsync(token));
        var body = json.RootElement;
        if (body.GetProperty("from").GetString() != owner || body.GetProperty("attachments")[0].GetString() != uploaded.Id)
            throw new InvalidOperationException("File changed mailbox or attachment.");
        var caption = body.GetProperty("body").GetString()!;
        var expected = sends++ == 0 ? "" : "Optional file caption";
        if (RequestEnvelope.DisplayText(caption) != expected || expected == "" && caption != "")
            throw new InvalidOperationException("Unexpected file caption.");
        var receipt = await preview.SendFileAsync(owner!, body.GetProperty("to").GetString()!, caption,
            body.TryGetProperty("thread_id", out var thread) ? thread.GetString() : null, uploaded, token);
        return Json(receipt);
    }
    private static HttpResponseMessage Json<T>(T value) => new(HttpStatusCode.Created)
    {
        Content = new StringContent(JsonSerializer.Serialize(value, PostboxJson.Options), System.Text.Encoding.UTF8, "application/json")
    };
}
#endif
