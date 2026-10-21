using System.Text.Json.Serialization;

namespace Pigeonpost.Core.Groups;

// The postbox's group and MLS routes (docs/groups.md §1–§3, §5) for one acting mailbox. Bytes
// travel as standard base64; the device header marks the device-scoped routes.
public sealed class GroupApi(PostboxClient postbox, string mailbox)
{
    public string Mailbox => mailbox;
    private static string G(string id) => "/v1/groups/" + Uri.EscapeDataString(id);

    // ---- devices and key packages ------------------------------------------------------------

    public Task<DeviceRegistration> RegisterDeviceAsync(string deviceId, byte[] signatureKey, CancellationToken ct) =>
        postbox.SendJsonAsync<DeviceRegistration>(HttpMethod.Post, "/v1/mls/devices",
            new { device_id = deviceId, signature_key = Convert.ToBase64String(signatureKey) }, mailbox, ct);

    public Task<DeviceList> DevicesAsync(string? ofMailbox, CancellationToken ct) =>
        postbox.GetJsonAsync<DeviceList>("/v1/mls/devices" + (ofMailbox is null ? "" : "?mailbox=" + Uri.EscapeDataString(ofMailbox)), mailbox, ct);

    public Task RevokeDeviceAsync(string deviceId, string actingDevice, CancellationToken ct) =>
        postbox.SendAsync(HttpMethod.Delete, "/v1/mls/devices/" + Uri.EscapeDataString(deviceId), null, mailbox, ct, actingDevice);

    public Task<KeyPackageCount> KeyPackageCountAsync(string device, CancellationToken ct) =>
        postbox.GetJsonAsync<KeyPackageCount>("/v1/mls/key-packages/count", mailbox, ct, device);

    public Task UploadKeyPackagesAsync(string device, IReadOnlyList<byte[]> packages, byte[]? lastResort, CancellationToken ct) =>
        postbox.SendAsync(HttpMethod.Post, "/v1/mls/key-packages", new KeyPackagesBody(RequestKey(), packages.Select(Convert.ToBase64String).ToArray(),
            lastResort is null ? null : Convert.ToBase64String(lastResort)), mailbox, ct, device);

    public async Task<IReadOnlyList<PendingWelcome>> WelcomesAsync(string device, CancellationToken ct) =>
        (await postbox.GetJsonAsync<WelcomesBody>("/v1/mls/welcomes", mailbox, ct, device)).Welcomes ?? [];

    public Task AckWelcomeAsync(string device, string welcomeId, bool joined, string? errorCode, CancellationToken ct) =>
        postbox.SendAsync(HttpMethod.Post, "/v1/mls/welcomes/" + Uri.EscapeDataString(welcomeId) + "/ack",
            new WelcomeAck(RequestKey(), joined, errorCode), mailbox, ct, device);

    // ---- groups ------------------------------------------------------------------------------

    public Task<CreatedGroup> CreateAsync(GroupSettings settings, IReadOnlyList<string> invite, CancellationToken ct) =>
        postbox.SendJsonAsync<CreatedGroup>(HttpMethod.Post, "/v1/groups", new CreateBody(RequestKey(), settings.Name, settings.Description,
            settings.JoinPolicy, settings.PostPolicy, settings.Visibility, settings.MembersCanInvite, settings.MembersCanChangeTimer,
            settings.EphemeralSecs, invite.Count == 0 ? null : invite), mailbox, ct);

    public async Task<IReadOnlyList<GroupInfo>> ListAsync(CancellationToken ct) =>
        (await postbox.GetJsonAsync<GroupsBody>("/v1/groups", mailbox, ct)).Groups ?? [];

    public Task<GroupInfo> GetAsync(string id, CancellationToken ct) => postbox.GetJsonAsync<GroupInfo>(G(id), mailbox, ct);

    public async Task<GroupInfo> UpdateAsync(string id, long expectedRevision, IReadOnlyDictionary<string, object?> changes, CancellationToken ct)
    {
        var body = new Dictionary<string, object?>(changes) { ["expected_revision"] = expectedRevision };
        return (await postbox.SendJsonAsync<GroupBody>(HttpMethod.Patch, G(id), body, mailbox, ct)).Group;
    }

    public Task DeleteAsync(string id, CancellationToken ct) => postbox.SendAsync(HttpMethod.Delete, G(id), null, mailbox, ct);

    public async Task<IReadOnlyList<GroupMember>> MembersAsync(string id, CancellationToken ct) =>
        (await postbox.GetJsonAsync<MembersBody>(G(id) + "/members", mailbox, ct)).Members ?? [];

    public async Task<IReadOnlyList<InviteResult>> InviteAsync(string id, IReadOnlyList<string> addresses, CancellationToken ct) =>
        (await postbox.SendJsonAsync<InviteBody>(HttpMethod.Post, G(id) + "/members", new { request_key = RequestKey(), invite = addresses }, mailbox, ct)).Results ?? [];

    public Task UpdateMemberAsync(string id, string address, long expectedRevision, string? role, string? state, CancellationToken ct) =>
        postbox.SendAsync(HttpMethod.Patch, G(id) + "/members/" + Uri.EscapeDataString(address), new { expected_revision = expectedRevision, role, state }, mailbox, ct);

    public Task AnswerInvitationAsync(string id, bool accept, CancellationToken ct) =>
        postbox.SendAsync(HttpMethod.Post, G(id) + "/invitation", new { accept }, mailbox, ct);

    public Task JoinAsync(string id, string? linkToken, CancellationToken ct) =>
        postbox.SendAsync(HttpMethod.Post, G(id) + "/join", new { link_token = linkToken }, mailbox, ct);

    public Task LeaveAsync(string id, CancellationToken ct) => postbox.SendAsync(HttpMethod.Post, G(id) + "/leave", new { }, mailbox, ct);

    public Task<GroupLink> SetLinkAsync(string id, bool enabled, CancellationToken ct) =>
        postbox.SendJsonAsync<GroupLink>(HttpMethod.Post, G(id) + "/link", new { enabled }, mailbox, ct);

    public Task MarkReadAsync(string id, long seq, CancellationToken ct) => postbox.SendAsync(HttpMethod.Post, G(id) + "/read", new { seq }, mailbox, ct);

    // ---- messages and handshakes -----------------------------------------------------------

    public Task<GroupMessagesPage> MessagesAsync(string id, string device, long afterSeq, CancellationToken ct) =>
        postbox.GetJsonAsync<GroupMessagesPage>(G(id) + "/messages?after_seq=" + afterSeq + "&limit=100", mailbox, ct, device);

    public Task<PostAccepted> PostAsync(string id, string device, PostBody body, CancellationToken ct) =>
        postbox.SendJsonAsync<PostAccepted>(HttpMethod.Post, G(id) + "/messages", body, mailbox, ct, device);

    public Task DeleteMessageAsync(string id, long seq, CancellationToken ct) =>
        postbox.SendAsync(HttpMethod.Delete, G(id) + "/messages/" + seq, null, mailbox, ct);

    public Task<UploadedAttachment> UploadAttachmentAsync(string id, string device, byte[] sealedBytes, CancellationToken ct) =>
        postbox.SendBytesAsync<UploadedAttachment>(HttpMethod.Post, G(id) + "/attachments", sealedBytes, "application/octet-stream", mailbox, ct, device);

    public Task<byte[]> DownloadAttachmentAsync(string id, string device, string attachmentId, CancellationToken ct) =>
        postbox.GetBytesAsync(G(id) + "/attachments/" + Uri.EscapeDataString(attachmentId), mailbox, ct, device);

    public Task<ClaimResult> ClaimAsync(string id, string device, IReadOnlyList<string> mailboxes, CancellationToken ct) =>
        postbox.SendJsonAsync<ClaimResult>(HttpMethod.Post, G(id) + "/key-packages/claim", new { request_key = RequestKey(), mailboxes }, mailbox, ct, device);

    public Task<CommitAccepted> CommitAsync(string id, string device, CommitBody body, CancellationToken ct) =>
        postbox.SendJsonAsync<CommitAccepted>(HttpMethod.Post, G(id) + "/commits", body, mailbox, ct, device);

    public Task EpochAckAsync(string id, string device, ulong epoch, CancellationToken ct) =>
        postbox.SendAsync(HttpMethod.Post, G(id) + "/epoch-ack", new { epoch }, mailbox, ct, device);

    public Task<CryptoStatus> CryptoAsync(string id, string device, CancellationToken ct) =>
        postbox.GetJsonAsync<CryptoStatus>(G(id) + "/crypto", mailbox, ct, device);

    public Task ResyncAsync(string id, string device, CancellationToken ct) =>
        postbox.SendAsync(HttpMethod.Post, G(id) + "/resync", new { request_key = RequestKey() }, mailbox, ct, device);

    public static string RequestKey() => "rk_" + RandomId(20);

    private const string Base32 = "abcdefghijklmnopqrstuvwxyz234567";
    public static string RandomId(int length)
    {
        var bytes = System.Security.Cryptography.RandomNumberGenerator.GetBytes(length);
        return new string(bytes.Select(b => Base32[b & 31]).ToArray());
    }

    private sealed record KeyPackagesBody(string RequestKey, string[] KeyPackages, string? LastResort);
    private sealed record WelcomesBody(IReadOnlyList<PendingWelcome>? Welcomes);
    private sealed record WelcomeAck(string RequestKey, bool Joined, string? ErrorCode);
    private sealed record CreateBody(string RequestKey, string Name, string? Description, string? JoinPolicy, string? PostPolicy,
        string? Visibility, bool? MembersCanInvite, bool? MembersCanChangeTimer, int? EphemeralSecs, IReadOnlyList<string>? Invite);
    private sealed record GroupsBody(IReadOnlyList<GroupInfo>? Groups);
    private sealed record GroupBody(GroupInfo Group);
    private sealed record MembersBody(IReadOnlyList<GroupMember>? Members);
    private sealed record InviteBody(IReadOnlyList<InviteResult>? Results);
}

public sealed record GroupSettings(string Name, string? Description = null, string? JoinPolicy = null, string? PostPolicy = null,
    string? Visibility = null, bool? MembersCanInvite = null, bool? MembersCanChangeTimer = null, int? EphemeralSecs = null);

public sealed record PostBody(string RequestKey, string ClientMessageId, ulong Epoch, string EncScheme, string Ciphertext, string EnvelopeSignature,
    IReadOnlyList<string> Attachments, long? ReplyTo = null, long? EditOf = null, IReadOnlyList<string>? Mentions = null);

public sealed record CommitBody(string RequestKey, ulong Epoch, string CommitId, string Commit, string EnvelopeSignature,
    [property: JsonIgnore(Condition = JsonIgnoreCondition.Never)] string? Welcome,
    IReadOnlyList<CommitAdd> Adds, IReadOnlyList<GroupDeviceRefJson> Removes, long? ExpectedRevision);

public sealed record CommitAdd(string Mailbox, string DeviceId, string KeyPackageRef);
