using System.Net;
using System.Net.Http.Headers;
using System.Net.Http.Json;
using System.Text.Json;

namespace Pigeonpost.Core;

public sealed class PostboxException(int statusCode, string? code) : Exception(code switch
{
    "not_admitted" => "They are not accepting mail from this mailbox.",
    "recipient_unresolved" => "No mailbox at that address.",
    "recipient_inbox_full" => "Their inbox is full.",
    "unauthorized" => "Your session expired. Sign in again.",
    "bad_response" => "The postbox returned an unreadable response.",
    _ => $"The postbox could not complete this request ({statusCode})."
})
{
    public int StatusCode { get; } = statusCode;
    public string? Code { get; } = code;
}

// HttpClient and token-provider lifetimes belong to the caller. Never log tokens or raw responses.
public sealed class PostboxClient(HttpClient http, IAccessTokenProvider tokens, Uri? endpoint = null) : IInboxService, IDisposable
{
    private readonly Uri endpoint = ValidateEndpoint(endpoint ?? new Uri("https://postbox.pigeonpost.dev"));
    private readonly SemaphoreSlim renewal = new(1, 1);

    public async Task<IReadOnlyList<Mailbox>> GetMailboxesAsync(CancellationToken cancellationToken)
    {
        var response = await ReadAsync<IdentitiesResponse>("/v1/identities", cancellationToken).ConfigureAwait(false);
        var result = new List<Mailbox>();
        foreach (var row in response.Identities ?? [])
        {
            var who = await ReadAsync<WhoAmI>(WithIdentity("/v1/whoami", row.Address), cancellationToken).ConfigureAwait(false);
            result.Add(new Mailbox(row.Address, who.Handle, row.Label));
        }
        return result;
    }

    public Task<InboxResponse> GetInboxAsync(string identity, int? wait = null, CancellationToken cancellationToken = default)
    {
        if (wait is < 0 or > 25) throw new ArgumentOutOfRangeException(nameof(wait));
        var path = WithIdentity("/v1/inbox", identity) + "&include_sent=true&include_read=true";
        if (wait is not null) path += "&wait=" + wait.Value;
        return ReadAsync<InboxResponse>(path, cancellationToken);
    }

    public async Task<InboxSnapshot> LoadAsync(string identity, CancellationToken cancellationToken)
    {
        var inbox = GetInboxAsync(identity, cancellationToken: cancellationToken);
        var threads = ReadAsync<ThreadsResponse>(WithIdentity("/v1/threads", identity), cancellationToken);
        var contacts = ReadAsync<ContactsResponse>(WithIdentity("/v1/contacts", identity), cancellationToken);
        var archive = ReadAsync<ArchiveResponse>(WithIdentity("/v1/archive", identity), cancellationToken);
        await Task.WhenAll(inbox, threads, contacts, archive).ConfigureAwait(false);
        return new InboxSnapshot((await inbox).Messages ?? [], (await threads).Threads ?? [],
            (await contacts).Contacts ?? [], new HashSet<string>((await archive).Archived ?? [], StringComparer.Ordinal));
    }

    public Task<SendReceipt> SendAsync(string identity, string peer, string body, string? threadId, CancellationToken cancellationToken) =>
        WriteAsync<SendReceipt>(HttpMethod.Post, "/v1/send", new { from = identity, to = peer, body, thread_id = threadId }, cancellationToken);

    public async Task<string> CreateThreadAsync(string identity, string peer, string title, CancellationToken cancellationToken) =>
        (await WriteAsync<OpenedThread>(HttpMethod.Post, "/v1/threads", new { identity, peer, title }, cancellationToken).ConfigureAwait(false)).ThreadId;

    public Task SetArchivedAsync(string identity, string peer, bool archived, CancellationToken cancellationToken) =>
        WriteWithoutResponseAsync(HttpMethod.Put, "/v1/archive", new { identity, peer, archived }, cancellationToken);

    public Task AcknowledgeAsync(string identity, string messageId, CancellationToken cancellationToken) =>
        WriteWithoutResponseAsync(HttpMethod.Post, "/v1/ack", new { identity, message_id = messageId }, cancellationToken);

    private async Task<T> ReadAsync<T>(string path, CancellationToken cancellationToken)
    {
        using var response = await RequestAsync(HttpMethod.Get, path, null, cancellationToken).ConfigureAwait(false);
        return await DecodeAsync<T>(response, cancellationToken).ConfigureAwait(false);
    }

    private async Task<T> WriteAsync<T>(HttpMethod method, string path, object body, CancellationToken cancellationToken)
    {
        using var response = await RequestAsync(method, path, JsonSerializer.Serialize(body, PostboxJson.Options), cancellationToken).ConfigureAwait(false);
        return await DecodeAsync<T>(response, cancellationToken).ConfigureAwait(false);
    }

    private async Task WriteWithoutResponseAsync(HttpMethod method, string path, object body, CancellationToken cancellationToken)
    {
        using var response = await RequestAsync(method, path, JsonSerializer.Serialize(body, PostboxJson.Options), cancellationToken).ConfigureAwait(false);
    }

    private async Task<HttpResponseMessage> RequestAsync(HttpMethod method, string path, string? json, CancellationToken cancellationToken)
    {
        var token = await tokens.GetTokenAsync(cancellationToken).ConfigureAwait(false);
        for (var attempt = 0; ; attempt++)
        {
            using var request = new HttpRequestMessage(method, new Uri(endpoint, path));
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", token);
            request.Headers.Accept.Add(new MediaTypeWithQualityHeaderValue("application/json"));
            if (json is not null) request.Content = new StringContent(json, System.Text.Encoding.UTF8, "application/json");
            var response = await http.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, cancellationToken).ConfigureAwait(false);
            if (response.StatusCode == HttpStatusCode.Unauthorized && attempt == 0)
            {
                response.Dispose();
                await renewal.WaitAsync(cancellationToken).ConfigureAwait(false);
                try
                {
                    var current = await tokens.GetTokenAsync(cancellationToken).ConfigureAwait(false);
                    token = current == token ? await tokens.RefreshTokenAsync(cancellationToken).ConfigureAwait(false) : current;
                }
                finally { renewal.Release(); }
                continue;
            }
            if (response.IsSuccessStatusCode) return response;
            using (response)
            {
                string? code = response.StatusCode == HttpStatusCode.Unauthorized ? "unauthorized" : null;
                try
                {
                    var error = await response.Content.ReadFromJsonAsync<ErrorResponse>(PostboxJson.Options, cancellationToken).ConfigureAwait(false);
                    code ??= error?.Error;
                }
                catch (JsonException) { }
                throw new PostboxException((int)response.StatusCode, code);
            }
        }
    }

    private static async Task<T> DecodeAsync<T>(HttpResponseMessage response, CancellationToken cancellationToken)
    {
        try
        {
            return await response.Content.ReadFromJsonAsync<T>(PostboxJson.Options, cancellationToken).ConfigureAwait(false)
                ?? throw new PostboxException((int)response.StatusCode, "bad_response");
        }
        catch (JsonException) { throw new PostboxException((int)response.StatusCode, "bad_response"); }
    }

    private static string WithIdentity(string path, string identity) => path + "?identity=" + Uri.EscapeDataString(identity);
    private static Uri ValidateEndpoint(Uri value) => value.IsAbsoluteUri && (value.Scheme == "https" || value.Scheme == "http" && value.IsLoopback)
        ? value : throw new ArgumentException("The postbox endpoint must use HTTPS (HTTP is allowed only on loopback).", nameof(value));
    public void Dispose() => renewal.Dispose();

    public sealed record InboxResponse(IReadOnlyList<InboxMessage>? Messages);
    private sealed record IdentityRow(string Address, string? Label);
    private sealed record IdentitiesResponse(IReadOnlyList<IdentityRow>? Identities);
    private sealed record WhoAmI(string? Handle);
    private sealed record ThreadsResponse(IReadOnlyList<ServerThread>? Threads);
    private sealed record ContactsResponse(IReadOnlyList<Contact>? Contacts);
    private sealed record ArchiveResponse(IReadOnlyList<string>? Archived);
    private sealed record OpenedThread(string ThreadId);
    private sealed record ErrorResponse(string? Error);
}
