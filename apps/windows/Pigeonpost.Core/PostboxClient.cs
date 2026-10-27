using System.Net;
using System.Net.Http.Headers;
using System.Net.Http.Json;
using System.Text.Json;

namespace Pigeonpost.Core;

public sealed class PostboxException(int statusCode, string? code, JsonElement? detail = null) : Exception(code switch
{
    "not_admitted" => "They are not accepting mail from this mailbox.",
    "recipient_unresolved" => "No mailbox at that address.",
    "recipient_inbox_full" => "Their inbox is full.",
    "unauthorized" => "Your session expired. Sign in again.",
    "attachment_too_large" => "This file exceeds the postbox's attachment size limit.",
    "empty_attachment" => "The selected file is empty.",
    "mailbox_full" => "Your mailbox has no room for this file.",
    "storage_full" => "The postbox has no room for more files. Please try again later.",
    "bad_response" => "The postbox returned an unreadable response.",
    "feature_disabled" => "This isn't available for your account yet.",
    "human_required" => "Only you can do this, signed in on this device.",
    "not_member" => "You are not an active member of this group.",
    "not_admin" => "Only the group's admins can do that.",
    "owner_only" => "Only the group's owner can do that.",
    "posting_restricted" => "Only admins can send messages in this group.",
    "banned" => "You can't join this group.",
    "group_full" => "This group is full.",
    "invalid_link" => "This invitation link is no longer valid.",
    "invite_required" => "This group is invite only.",
    "group_deleted" => "This group was deleted.",
    "rate_limited" => "Too many requests. Please wait a moment and try again.",
    "quota_exceeded" => "This group has no room for more files.",
    "avatar_rate_limited" => "You can change your picture five times a day. Please try again tomorrow.",
    "too_large" => "This picture is too large. Choose one under 10 MB.",
    "unsupported_image" => "Choose a JPEG, PNG or WebP picture.",
    "bad_image" => "This picture could not be read. Please choose another.",
    "avatars_unavailable" => "Pictures can't be changed right now. Please try again later.",
    "invalid_policy" => "That setting could not be saved.",
    _ => $"The postbox could not complete this request ({statusCode})."
})
{
    public int StatusCode { get; } = statusCode;
    public string? Code { get; } = code;
    // The whole error body, for codes that carry fields (current_epoch, current_revision, …).
    public JsonElement? Detail { get; } = detail;
    public long? DetailNumber(string name) => Detail is { ValueKind: JsonValueKind.Object } body
        && body.TryGetProperty(name, out var value) && value.TryGetInt64(out var number) ? number : null;
}

// HttpClient and token-provider lifetimes belong to the caller. Never log tokens or raw responses.
public sealed partial class PostboxClient(HttpClient http, IAccessTokenProvider tokens, Uri? endpoint = null, HttpClient? transfers = null) : IInboxService, IAttachmentService, IConversationSettings, IDisposable
{
    private readonly Uri endpoint = ValidateEndpoint(endpoint ?? new Uri("https://postbox.pigeonpost.dev"));
    private readonly SemaphoreSlim renewal = new(1, 1);
    public const int MaxAttachmentBytes = 25 * 1024 * 1024;
    internal AuthorizationLease? AcceptedAuthorization => tokens.AcceptedAuthorization;
    AuthorizationLease? IInboxService.AcceptedAuthorization => AcceptedAuthorization;
    string? IInboxService.RecoveryOwnerKey => tokens.RecoveryOwnerKey;
    string? IInboxService.RecoveryOrigin => endpoint.GetLeftPart(UriPartial.Authority);
    AuthorizationLease? IAttachmentService.AcceptedAuthorization => AcceptedAuthorization;
    AuthorizationLease? IConversationSettings.AcceptedAuthorization => AcceptedAuthorization;
    internal Action CaptureAuthorizationValidation() => AuthorizationContext.CaptureValidation(tokens);

    public async Task<IReadOnlyList<Mailbox>> GetMailboxesAsync(CancellationToken cancellationToken)
    {
        using var operation = AuthorizationContext.Capture(tokens.AcceptedAuthorization);
        var owner = new RequestAuthorization(tokens);
        var response = await ReadAsync<IdentitiesResponse>("/v1/identities", cancellationToken).ConfigureAwait(false);
        var result = new List<Mailbox>();
        foreach (var row in response.Identities ?? [])
        {
            string? handle = null;
            try { handle = (await ReadAsync<WhoAmI>(WithIdentity("/v1/whoami", row.Address), cancellationToken).ConfigureAwait(false)).Handle; }
            catch (PostboxException ex) when (ex.StatusCode is not 401 and not 403) { }
            result.Add(new Mailbox(row.Address, handle, row.Label));
        }
        owner.Check(cancellationToken);
        return result;
    }

    public async Task<string> CreateMailboxAsync(CancellationToken cancellationToken) =>
        (await WriteAsync<CreatedIdentity>(HttpMethod.Post, "/v1/identities", new { }, cancellationToken).ConfigureAwait(false)).Address;

    public Task<InboxResponse> GetInboxAsync(string identity, int? wait = null, CancellationToken cancellationToken = default)
    {
        if (wait is < 0 or > 25) throw new ArgumentOutOfRangeException(nameof(wait));
        var path = WithIdentity("/v1/inbox", identity) + "&include_sent=true&include_read=true&actions=1&fold=1";
        if (wait is not null) path += "&wait=" + wait.Value;
        return ReadAsync<InboxResponse>(path, cancellationToken);
    }

    // One account event stream wakes the desktop for every mailbox. Periodic refresh remains
    // the fallback for read-state changes and networks that block long-lived connections.
    public async Task WatchMailAsync(Func<Task> changed, CancellationToken cancellationToken, Action<string>? log = null)
    {
        using var operation = AuthorizationContext.Capture(tokens.AcceptedAuthorization);
        var owner = new RequestAuthorization(tokens);
        long? cursor = null;
        while (!cancellationToken.IsCancellationRequested)
        {
            try
            {
                var path = "/v1/events" + (cursor is { } id ? "?last_event_id=" + id : "");
                using var request = new HttpRequestMessage(HttpMethod.Get, new Uri(endpoint, path));
                owner.Check(cancellationToken);
                var token = await tokens.GetTokenAsync(cancellationToken).ConfigureAwait(false);
                owner.Check(cancellationToken);
                request.Headers.Authorization = new("Bearer", token);
                request.Headers.Accept.Add(new("text/event-stream"));
                using var response = await http.SendAsync(request, HttpCompletionOption.ResponseHeadersRead, cancellationToken).ConfigureAwait(false);
                owner.Check(cancellationToken);
                if (response.StatusCode == HttpStatusCode.Unauthorized)
                {
                    await tokens.RefreshTokenAsync(cancellationToken).ConfigureAwait(false);
                    owner.Check(cancellationToken);
                }
                else
                {
                    response.EnsureSuccessStatusCode();
                    if (response.Content.Headers.ContentType?.MediaType != "text/event-stream") throw new IOException("Invalid event stream.");
                    log?.Invoke("Account event stream connected");
                    using var stream = await response.Content.ReadAsStreamAsync(cancellationToken).ConfigureAwait(false);
                    using var reader = new StreamReader(stream);
                    using var idle = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken);
                    string? eventName = null;
                    long? eventId = null;
                    while (!cancellationToken.IsCancellationRequested)
                    {
                        idle.CancelAfter(TimeSpan.FromSeconds(45));
                        var line = await reader.ReadLineAsync(idle.Token).ConfigureAwait(false);
                        owner.Check(cancellationToken);
                        if (line is null) break;
                        if (line.Length > 16384) throw new IOException("Event exceeds the supported size.");
                        if (line.Length == 0)
                        {
                            if (eventName == "mail") { log?.Invoke("Account event: new mail"); await changed().ConfigureAwait(false); }
                            if (cancellationToken.IsCancellationRequested) return;
                            owner.Check(cancellationToken);
                            if (eventId is { } next) cursor = next;
                            eventName = null; eventId = null;
                        }
                        else if (line.StartsWith("event:")) eventName = line[6..].Trim();
                        else if (line.StartsWith("id:") && long.TryParse(line[3..].Trim(), out var next) && next >= 0) eventId = next;
                    }
                }
            }
            catch (AuthorizationRetiredException) { throw; }
            catch (Exception ex) when (!cancellationToken.IsCancellationRequested)
            {
                // Offline: reconnect with the last processed cursor.
                log?.Invoke($"Account event stream interrupted: {ex.GetType().Name} {(ex as HttpRequestException)?.StatusCode}");
            }
            if (!cancellationToken.IsCancellationRequested)
                await Task.Delay(TimeSpan.FromSeconds(5), cancellationToken).ConfigureAwait(false);
        }
    }

    public async Task<InboxSnapshot> LoadAsync(string identity, CancellationToken cancellationToken)
    {
        using var operation = AuthorizationContext.Capture(tokens.AcceptedAuthorization);
        var owner = new RequestAuthorization(tokens);
        var inbox = GetInboxAsync(identity, cancellationToken: cancellationToken);
        var threads = ReadAsync<ThreadsResponse>(WithIdentity("/v1/threads", identity), cancellationToken);
        var contacts = ReadAsync<ContactsResponse>(WithIdentity("/v1/contacts", identity), cancellationToken);
        var archive = ReadAsync<ArchiveResponse>(WithIdentity("/v1/archive", identity), cancellationToken);
        await Task.WhenAll(inbox, threads, contacts, archive).ConfigureAwait(false);
        owner.Check(cancellationToken);
        return new InboxSnapshot((await inbox).Messages ?? [], (await threads).Threads ?? [],
            (await contacts).Contacts ?? [], new HashSet<string>((await archive).Archived ?? [], StringComparer.Ordinal), (await contacts).Vocabulary);
    }

    public Task<SendReceipt> SendAsync(string identity, string peer, string body, string? threadId, CancellationToken cancellationToken) =>
        WriteAsync<SendReceipt>(HttpMethod.Post, "/v1/send", new { from = identity, to = peer, body, thread_id = threadId }, cancellationToken);

    public async Task<string> CreateThreadAsync(string identity, string peer, string title, CancellationToken cancellationToken) =>
        (await WriteAsync<OpenedThread>(HttpMethod.Post, "/v1/threads", new { identity, peer, title }, cancellationToken).ConfigureAwait(false)).ThreadId;

    public Task SetArchivedAsync(string identity, string peer, bool archived, CancellationToken cancellationToken) =>
        WriteWithoutResponseAsync(HttpMethod.Put, "/v1/archive", new { identity, peer, archived }, cancellationToken);

    public Task AcknowledgeAsync(string identity, string messageId, CancellationToken cancellationToken) =>
        WriteWithoutResponseAsync(HttpMethod.Post, "/v1/ack", new { identity, message_id = messageId, with_edits = true }, cancellationToken);

    public Task SetContactAsync(string identity, Contact contact, CancellationToken cancellationToken) =>
        WriteWithoutResponseAsync(HttpMethod.Put, "/v1/contacts", new
        {
            identity, contact.Peer, contact.Alias, contact.Admission, contact.Autonomy, contact.AllowedVerbs
        }, cancellationToken);

    public Task RemoveContactAsync(string identity, string peer, CancellationToken cancellationToken) =>
        WriteWithoutResponseAsync(HttpMethod.Put, "/v1/contacts", new { identity, peer, remove = true }, cancellationToken);

    public async Task DeleteThreadAsync(string identity, string threadId, CancellationToken cancellationToken)
    {
        using var response = await RequestAsync(HttpMethod.Delete,
            WithIdentity("/v1/threads/" + Uri.EscapeDataString(threadId), identity), null, cancellationToken).ConfigureAwait(false);
    }

    // Incoming-call alerts while the app is closed (docs/call-alerting.md): the WNS channel URI is the
    // device token. The postbox wakes devices of the mailbox or of the account that owns it.
    public Task RegisterDeviceAsync(string identity, string token, string platform, string os, CancellationToken cancellationToken, string kind = "alert") =>
        WriteWithoutResponseAsync(HttpMethod.Post, "/v1/devices", new { token, platform, kind, os, identity }, cancellationToken);

    public async Task UnregisterDeviceAsync(string token, CancellationToken cancellationToken)
    {
        using var response = await RequestAsync(HttpMethod.Delete, "/v1/devices/" + Uri.EscapeDataString(token), null, cancellationToken).ConfigureAwait(false);
    }

    public async Task<MessageAttachment> UploadAsync(string identity, string filename, byte[] bytes, CancellationToken cancellationToken)
    {
        if (bytes.Length is 0 or > MaxAttachmentBytes) throw new ArgumentException("Choose a file between 1 byte and 25 MB.");
        var safeName = new string(filename.Select(c => c is >= ' ' and <= '~' && c is not '\r' and not '\n' and not '/' and not '\\' ? c : '_').Take(120).ToArray());
        using var response = await RequestAsync(HttpMethod.Post, "/v1/attachments", null, cancellationToken, bytes, identity, safeName, transfer: true).ConfigureAwait(false);
        return await DecodeAsync<MessageAttachment>(response, cancellationToken).ConfigureAwait(false);
    }

    public async Task<byte[]> DownloadAsync(string identity, string attachmentId, CancellationToken cancellationToken)
    {
        using var response = await RequestAsync(HttpMethod.Get, "/v1/attachments/" + Uri.EscapeDataString(attachmentId), null, cancellationToken, identity: identity, transfer: true).ConfigureAwait(false);
        await response.Content.LoadIntoBufferAsync(MaxAttachmentBytes, cancellationToken).ConfigureAwait(false);
        var bytes = await response.Content.ReadAsByteArrayAsync(cancellationToken).ConfigureAwait(false);
        CheckResponseAuthorization(response, cancellationToken);
        return bytes;
    }

    public Task<SendReceipt> SendAttachmentAsync(string identity, string peer, string body, string? threadId, string attachmentId, CancellationToken cancellationToken) =>
        SendAttachmentsAsync(identity, peer, body, threadId, [attachmentId], cancellationToken);

    public Task<SendReceipt> SendAttachmentsAsync(string identity, string peer, string body, string? threadId, IReadOnlyList<string> attachmentIds, CancellationToken cancellationToken) =>
        WriteAsync<SendReceipt>(HttpMethod.Post, "/v1/send", new { from = identity, to = peer, body, thread_id = threadId, attachments = attachmentIds }, cancellationToken);

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

    private async Task<HttpResponseMessage> RequestAsync(HttpMethod method, string path, string? json, CancellationToken cancellationToken,
        byte[]? binary = null, string? identity = null, string? filename = null, bool transfer = false, string? mediaType = null, string? device = null)
    {
        var owner = new RequestAuthorization(tokens);
        owner.Check(cancellationToken);
        var token = await tokens.GetTokenAsync(cancellationToken).ConfigureAwait(false);
        owner.Check(cancellationToken);
        for (var attempt = 0; ; attempt++)
        {
            using var request = new HttpRequestMessage(method, new Uri(endpoint, path));
            request.Options.Set(AuthorizationKey, owner);
            request.Headers.Authorization = new AuthenticationHeaderValue("Bearer", token);
            request.Headers.Accept.Add(new MediaTypeWithQualityHeaderValue("application/json"));
            if (identity is not null) request.Headers.Add("x-pigeonpost-identity", identity);
            if (device is not null) request.Headers.Add("x-pigeonpost-device", device);
            if (binary is not null)
            {
                request.Content = new ByteArrayContent(binary);
                request.Content.Headers.ContentType = new MediaTypeHeaderValue(mediaType ?? "application/octet-stream");
                if (filename is not null)
                {
                    request.Headers.Add("x-pigeonpost-filename", filename);
                    request.Headers.Add("x-pigeonpost-media-type", "application/octet-stream");
                }
            }
            if (json is not null) request.Content = new StringContent(json, System.Text.Encoding.UTF8, "application/json");
            owner.Check(cancellationToken);
            var response = await (transfer ? transfers ?? http : http).SendAsync(request,
                HttpCompletionOption.ResponseHeadersRead, cancellationToken).ConfigureAwait(false);
            response.RequestMessage = request;
            try { owner.Check(cancellationToken); }
            catch { response.Dispose(); throw; }
            if (response.StatusCode == HttpStatusCode.Unauthorized && attempt == 0)
            {
                response.Dispose();
                await renewal.WaitAsync(cancellationToken).ConfigureAwait(false);
                try
                {
                    owner.Check(cancellationToken);
                    var current = await tokens.GetTokenAsync(cancellationToken).ConfigureAwait(false);
                    owner.Check(cancellationToken);
                    token = current == token ? await tokens.RefreshTokenAsync(cancellationToken).ConfigureAwait(false) : current;
                    owner.Check(cancellationToken);
                }
                finally { renewal.Release(); }
                continue;
            }
            if (response.IsSuccessStatusCode) return response;
            using (response)
            {
                string? code = response.StatusCode == HttpStatusCode.Unauthorized ? "unauthorized" : null;
                JsonElement? detail = null;
                try
                {
                    var error = await response.Content.ReadFromJsonAsync<JsonElement>(PostboxJson.Options, cancellationToken).ConfigureAwait(false);
                    if (error.ValueKind == JsonValueKind.Object)
                    {
                        detail = error;
                        if (error.TryGetProperty("error", out var name) && name.ValueKind == JsonValueKind.String) code ??= name.GetString();
                    }
                }
                catch (Exception ex) when (ex is JsonException or NotSupportedException or InvalidOperationException) { }
                owner.Check(cancellationToken);
                throw new PostboxException((int)response.StatusCode, code, detail);
            }
        }
    }

    private static async Task<T> DecodeAsync<T>(HttpResponseMessage response, CancellationToken cancellationToken)
    {
        CheckResponseAuthorization(response, cancellationToken);
        try
        {
            var value = await response.Content.ReadFromJsonAsync<T>(PostboxJson.Options, cancellationToken).ConfigureAwait(false)
                ?? throw new PostboxException((int)response.StatusCode, "bad_response");
            CheckResponseAuthorization(response, cancellationToken);
            return value;
        }
        catch (JsonException) { CheckResponseAuthorization(response, cancellationToken); throw new PostboxException((int)response.StatusCode, "bad_response"); }
    }

    private static readonly HttpRequestOptionsKey<RequestAuthorization> AuthorizationKey = new("Pigeonpost.LocalAuthorization");
    private sealed class RequestAuthorization(IAccessTokenProvider provider)
    {
        private readonly Action validate = AuthorizationContext.CaptureValidation(provider);
        public void Check(CancellationToken cancellationToken)
        {
            cancellationToken.ThrowIfCancellationRequested();
            validate();
        }
    }
    private static void CheckResponseAuthorization(HttpResponseMessage response, CancellationToken cancellationToken)
    {
        cancellationToken.ThrowIfCancellationRequested();
        if (response.RequestMessage?.Options.TryGetValue(AuthorizationKey, out var owner) == true) owner.Check(cancellationToken);
    }

    private static string WithIdentity(string path, string identity) => path + "?identity=" + Uri.EscapeDataString(identity);
    private static Uri ValidateEndpoint(Uri value) => value.IsAbsoluteUri && (value.Scheme == "https" || value.Scheme == "http" && value.IsLoopback)
        ? value : throw new ArgumentException("The postbox endpoint must use HTTPS (HTTP is allowed only on loopback).", nameof(value));
    public void Dispose() => renewal.Dispose();

    public sealed record InboxResponse(IReadOnlyList<InboxMessage>? Messages);
    private sealed record IdentityRow(string Address, string? Label);
    private sealed record CreatedIdentity(string Address);
    private sealed record IdentitiesResponse(IReadOnlyList<IdentityRow>? Identities);
    private sealed record WhoAmI(string? Handle);
    private sealed record ThreadsResponse(IReadOnlyList<ServerThread>? Threads);
    private sealed record ContactsResponse(IReadOnlyList<Contact>? Contacts, Vocabulary? Vocabulary);
    private sealed record ArchiveResponse(IReadOnlyList<string>? Archived);
    private sealed record OpenedThread(string ThreadId);
}
