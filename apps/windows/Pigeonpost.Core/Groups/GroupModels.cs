using System.Text.Json;
using System.Text.Json.Serialization;

namespace Pigeonpost.Core.Groups;

// Server records of docs/groups.md (Phase 3a). Every field the server may leave out is nullable:
// an invited mailbox sees only {id, name, description, member_count, join_policy, my}.

public sealed record GroupMy
{
    public string? Role { get; init; }
    public string? State { get; init; }
    public long? ReadCursor { get; init; }
    public int? Unread { get; init; }
    public string? Notify { get; init; }
    public long? MutedUntil { get; init; }
    public string? InvitedBy { get; init; }
    public long? JoinedAt { get; init; }
    public bool? Filtered { get; init; }
    public long? Revision { get; init; }
}

public sealed record GroupInfo
{
    public required string Id { get; init; }
    public string? Name { get; init; }
    public string? Description { get; init; }
    public string? AvatarId { get; init; }
    public string? JoinPolicy { get; init; }
    public string? PostPolicy { get; init; }
    public string? Visibility { get; init; }
    public bool? MembersCanInvite { get; init; }
    public bool? MembersCanChangeTimer { get; init; }
    [JsonIgnore(Condition = JsonIgnoreCondition.Never)] public int? EphemeralSecs { get; init; }
    public long? Revision { get; init; }
    public long? Epoch { get; init; }
    public string? ActivationState { get; init; }
    public int? MemberCount { get; init; }
    public int? MaxMembers { get; init; }
    public string? CreatedBy { get; init; }
    public long? CreatedAt { get; init; }
    public long? LastAt { get; init; }
    public long? LastSeq { get; init; }
    public long? DeletedAt { get; init; }
    public bool? HasLink { get; init; }
    public GroupMy? My { get; init; }

    [JsonIgnore] public string Role => My?.Role ?? "member";
    [JsonIgnore] public string State => My?.State ?? "active";
    [JsonIgnore] public bool IsAdmin => Role is "owner" or "admin";
    [JsonIgnore] public bool IsOwner => Role == "owner";
    [JsonIgnore] public bool IsInvited => State == "invited";
    [JsonIgnore] public bool IsActive => State == "active" && DeletedAt is null;
    [JsonIgnore] public bool IsKeying => State == "keying";
    [JsonIgnore] public bool IsDeleted => DeletedAt is not null;
    [JsonIgnore] public bool AnnouncementOnly => PostPolicy == "admins";
    [JsonIgnore] public bool CanPost => IsActive && Role != "restricted" && (!AnnouncementOnly || IsAdmin);
    [JsonIgnore] public bool CanInvite => IsActive && (IsAdmin || MembersCanInvite == true);
    [JsonIgnore] public bool CanChangeTimer => IsActive && (IsAdmin || MembersCanChangeTimer == true);
    [JsonIgnore] public string DisplayName => string.IsNullOrWhiteSpace(Name) ? "Group" : Name!;
}

public sealed record GroupMember
{
    public required string Address { get; init; }
    public string? Handle { get; init; }
    public string? Role { get; init; }
    public string? State { get; init; }
    public string? InvitedBy { get; init; }
    public long? JoinedAt { get; init; }
    public long? Revision { get; init; }
    [JsonIgnore] public string Key => Handle ?? Address;
}

public sealed record GroupAttachmentStub(string Id, long? Bytes = null);
public sealed record GroupDeviceRefJson(string Mailbox, string DeviceId);

public sealed record GroupItem
{
    public long Seq { get; init; }
    public string? Type { get; init; }
    public string? Id { get; init; }
    public long? Epoch { get; init; }
    public string? Sender { get; init; }
    public string? SenderDevice { get; init; }
    public string? ClientMessageId { get; init; }
    public string? Ciphertext { get; init; }
    public IReadOnlyList<GroupAttachmentStub>? Attachments { get; init; }
    public long? ReplyTo { get; init; }
    public long? EditOf { get; init; }
    public long? CreatedAt { get; init; }
    public long? ExpiresAt { get; init; }
    public long? DeletedAt { get; init; }
    public long? ReplacedBy { get; init; }
    public string? CommitId { get; init; }
    public string? Commit { get; init; }
    public IReadOnlyList<GroupDeviceRefJson>? Adds { get; init; }
    public IReadOnlyList<GroupDeviceRefJson>? Removes { get; init; }
    public string? Kind { get; init; }
    public string? Actor { get; init; }
    public JsonElement? Metadata { get; init; }
}

public sealed record GroupMessagesPage(IReadOnlyList<GroupItem>? Items, long? LastSeq, long? Epoch, bool? More);
public sealed record PendingChange(string Kind, string Mailbox, string DeviceId, long? Since = null);
public sealed record LaggingDevice(string Mailbox, string DeviceId, long? AckedEpoch);
public sealed record CryptoStatus(long? Epoch, long? Revision, string? ActivationState, bool? InGroup,
    IReadOnlyList<PendingChange>? Pending, IReadOnlyList<LaggingDevice>? LaggingDevices);
public sealed record DeviceRegistration(string DeviceId, string? Fingerprint, string? FingerprintDigits, string Attestation, string MailboxPublicKey, long? CreatedAt);
public sealed record RegisteredDevice(string DeviceId, string? Fingerprint, string? FingerprintDigits, string? SignatureKey, long? CreatedAt);
public sealed record DeviceList(string? Mailbox, IReadOnlyList<RegisteredDevice>? Devices);
public sealed record KeyPackageCount(int Unclaimed, bool LastResort);
public sealed record PendingWelcome(string WelcomeId, string GroupId, long? Epoch, long? Seq, string Welcome);
public sealed record KeyPackageClaim(string Mailbox, string DeviceId, string KeyPackage, string KeyPackageRef);
public sealed record ClaimResult(IReadOnlyList<KeyPackageClaim>? Claims, IReadOnlyList<GroupDeviceRefJson>? Exhausted);
public sealed record CommitAccepted(long Seq, long Epoch, long? Revision);
public sealed record PostAccepted(long Seq, string? Id, long? ExpiresAt);
public sealed record UploadedAttachment(string Id, long? Bytes, string? Sha256);
public sealed record InviteResult(string Address, string Status);
public sealed record CreatedGroup(GroupInfo Group, IReadOnlyList<InviteResult>? Invitations);
public sealed record GroupLink(string? LinkToken, string? Url);
public sealed record GroupCallStatus(bool Active, string? Room = null, string? Kind = null, IReadOnlyList<string>? Participants = null);

// ---- local, decrypted history (only ever on this device, encrypted at rest) ------------------

public sealed record DeviceSummary(string Mailbox, string DeviceId, string Fingerprint);

public sealed class LocalFile
{
    public string Id { get; set; } = "";
    public string Name { get; set; } = "file";
    public string Mime { get; set; } = "application/octet-stream";
    public long Size { get; set; }
    public string? Key { get; set; }
    public string? Sha { get; set; }
    public ulong Len { get; set; }
}

// One line of a group as this device saw it: a message, a server notice, a verified commit or a
// warning. Kept in seq order; warnings sort just after the item they are about.
public sealed class GroupLine
{
    public string Kind { get; set; } = "message"; // message | notice | commit | warning
    public long Seq { get; set; }
    public int Order { get; set; }
    public string? Sender { get; set; }
    public string? SenderDevice { get; set; }
    public string? Cmid { get; set; }
    public string? Text { get; set; }
    public List<LocalFile>? Files { get; set; }
    public string? ReplyTo { get; set; }
    public string? EditOf { get; set; }
    public long CreatedAt { get; set; }
    public long? ExpiresAt { get; set; }
    public bool Deleted { get; set; }
    public long? ReplacedBy { get; set; }
    public bool Undecryptable { get; set; }
    public string? Code { get; set; }
    public bool Pending { get; set; }
    public bool Mine { get; set; }
    public string? Notice { get; set; }
    public string? Actor { get; set; }
    public Dictionary<string, string>? Metadata { get; set; }
    public long? Epoch { get; set; }
    public DeviceSummary? Committer { get; set; }
    public List<DeviceSummary>? Added { get; set; }
    public List<DeviceSummary>? Removed { get; set; }
    public List<DeviceSummary>? Updated { get; set; }
    public string? Warning { get; set; }
    public bool Local { get; set; }
}

public sealed class GroupCursor
{
    public long Cursor { get; set; }
    public bool InGroup { get; set; }
    public ulong Acked { get; set; }
    public string? SafetyNumber { get; set; }
    // mailbox|device → fingerprint, as of the last verified epoch: device-change notices compare with it.
    public Dictionary<string, string>? Devices { get; set; }
}

// Everything this device keeps for one mailbox: the MLS state (already encrypted under StateKey)
// and its decrypted history. The whole record is encrypted again at rest (IStateProtector).
public sealed class GroupDeviceRecord
{
    public string DeviceId { get; set; } = "";
    public bool Registered { get; set; }
    public string StateKey { get; set; } = "";
    public string? StateBlob { get; set; }
    public string? Fingerprint { get; set; }
    public Dictionary<string, GroupCursor> Groups { get; set; } = new(StringComparer.Ordinal);
    public Dictionary<string, List<GroupLine>> History { get; set; } = new(StringComparer.Ordinal);
}
