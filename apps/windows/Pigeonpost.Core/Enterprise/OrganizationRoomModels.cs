using System.Globalization;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;

namespace Pigeonpost.Core.Enterprise;

public enum RoomJoinPolicy { Members, Invited }
public enum RoomPostPolicy { Everyone, Admins }
public enum RoomAction { Open, AcceptInvitation, Join, Unavailable }

public static class OrganizationRoomInput
{
    public static bool Identifier(string? value) => value is { Length: >= 1 and <= 128 }
        && value.All(c => char.IsAsciiLetterOrDigit(c) || c is '_' or '-');
    public static bool Mailbox(string? value) => value is not null && MessageRecoveryScope.CanonicalMailbox(value);
    public static bool Text(string? value, int maximum, bool multiline = false)
    {
        if (value is null) return false;
        var count = 0;
        foreach (var rune in value.EnumerateRunes())
        {
            if (++count > maximum) return false;
            var v = rune.Value;
            if (v is >= 0x202A and <= 0x202E or >= 0x2066 and <= 0x2069 or 0x200E or 0x200F or 0x061C) return false;
            if (Rune.IsControl(rune) && !(multiline && v is 10 or 9)) return false;
        }
        // Unpaired surrogates are not runes; reject them explicitly.
        for (var i = 0; i < value.Length; i++)
            if (char.IsSurrogate(value[i]) && !(char.IsHighSurrogate(value[i]) && i + 1 < value.Length && char.IsLowSurrogate(value[++i]))) return false;
        return true;
    }
}

public sealed record RoomMailbox(string Address, string? Handle = null)
{
    public bool Valid => OrganizationRoomInput.Mailbox(Address) && (Handle is null || OrganizationRoomInput.Text(Handle, 256));
}

public sealed record RoomOrganization(string Id, string Name, string Status, string MemberId, string? Namespace,
    string? RoomsCreation, IReadOnlyList<string> Roles, RoomMailbox? Mailbox)
{
    public bool Readable => Status is "active" or "past_due" or "read_only";
    public bool Writable => Status is "active" or "past_due";
    public bool Valid => OrganizationRoomInput.Identifier(Id) && OrganizationRoomInput.Identifier(MemberId) && !string.IsNullOrWhiteSpace(Name)
        && OrganizationRoomInput.Text(Name, 256) && OrganizationRoomInput.Identifier(Status) && Mailbox?.Valid != false
        && (Namespace is null || OrganizationRoomInput.Text(Namespace, 128)) && Roles.Count <= 16
        && Roles.Distinct(StringComparer.Ordinal).Count() == Roles.Count && Roles.All(OrganizationRoomInput.Identifier);
    // A display hint only: the server rechecks current membership and policy on creation.
    public bool MayCreate(string acting) => Writable && Mailbox?.Address == acting && RoomsCreation switch
    {
        "everyone" => true,
        "admins" => Roles.Any(r => r is "owner" or "admin" or "people_admin" or "agent_admin" or "billing_admin"),
        _ => false
    };
    public override string ToString() => "RoomOrganization(redacted)";
}

public sealed record RoomOperatedMailbox(string Address, string OrganizationId, string? Handle, IReadOnlyList<string> Capabilities)
{
    public bool Valid => OrganizationRoomInput.Mailbox(Address) && OrganizationRoomInput.Identifier(OrganizationId)
        && (Handle is null || OrganizationRoomInput.Text(Handle, 256)) && Capabilities.Count <= 16
        && Capabilities.Distinct(StringComparer.Ordinal).Count() == Capabilities.Count && Capabilities.All(OrganizationRoomInput.Identifier);
}

public sealed record RoomOrganizations(IReadOnlyList<RoomOrganization> Orgs, IReadOnlyList<RoomOperatedMailbox> Operated)
{
    public bool Valid => Orgs.Count <= 1000 && Orgs.All(o => o.Valid) && Orgs.Select(o => o.Id).Distinct(StringComparer.Ordinal).Count() == Orgs.Count
        && Operated.Count <= 10_000 && Operated.All(o => o.Valid && Orgs.Any(org => org.Id == o.OrganizationId));
    // Canonical membership only: the organization's own member inbox for this person. Handles,
    // labels and cached account metadata never establish it.
    public IReadOnlyList<RoomOrganization> OwnedFor(string mailbox) => !Valid || !OrganizationRoomInput.Mailbox(mailbox) ? []
        : Orgs.Where(o => o.Readable && o.Mailbox?.Address == mailbox).ToArray();
    // Explicitly delegated agents (read_inbox). Listed for transparency; Windows does not open them yet.
    public IReadOnlyList<RoomOperatedMailbox> Delegated => !Valid ? [] : Operated.Where(o => o.Capabilities.Contains("read_inbox")).ToArray();
    public override string ToString() => "RoomOrganizations(redacted)";
}

public sealed record OrganizationRoom(string Id, string Name, string? Description, int MemberCount, string JoinPolicy, string? Membership)
{
    public bool Valid => OrganizationRoomInput.Identifier(Id) && !string.IsNullOrWhiteSpace(Name) && OrganizationRoomInput.Text(Name, 256)
        && (Description is null || OrganizationRoomInput.Text(Description, 2000, multiline: true)) && MemberCount >= 0
        && OrganizationRoomInput.Identifier(JoinPolicy) && (Membership is null || OrganizationRoomInput.Identifier(Membership));
    public RoomAction Action => Membership switch
    {
        "active" or "keying" => RoomAction.Open,
        "invited" => RoomAction.AcceptInvitation,
        null or "left" or "removed" => JoinPolicy == "public" ? RoomAction.Join : RoomAction.Unavailable,
        _ => RoomAction.Unavailable
    };
}

public sealed record RoomPreview(string Id, string Name, string? Description, int MemberCount, string JoinPolicy, string? MyState, bool CanJoin)
{
    public bool Valid => OrganizationRoomInput.Identifier(Id) && OrganizationRoomInput.Text(Name, 256) && MemberCount >= 0
        && (Description is null || OrganizationRoomInput.Text(Description, 2000, multiline: true)) && OrganizationRoomInput.Identifier(JoinPolicy)
        && (MyState is null || OrganizationRoomInput.Identifier(MyState));
}

// A room confirmed by a fresh single-group read: explicit organization, live state and role.
public sealed record ConfirmedRoom(string Id, string OrganizationId, string Name, string State, string Role, string? CreatedBy, string? ActivationState);

// Exact request identity and bytes stay unchanged through an explicit uncertain-outcome retry.
public sealed class OrganizationRoomCreate
{
    public string OrganizationId { get; }
    public string Name { get; }
    public string Description { get; }
    public RoomJoinPolicy JoinPolicy { get; }
    public RoomPostPolicy PostPolicy { get; }
    public bool Listed { get; }
    public string RequestKey { get; }
    public string Encoded { get; }
    public OrganizationRoomCreate(string organizationId, string name, string description, RoomJoinPolicy join, RoomPostPolicy post, bool listed, string requestKey)
    {
        name = name.Trim(); description = description.Trim();
        if (!OrganizationRoomInput.Identifier(organizationId)) throw new ArgumentException("Choose a valid organization.");
        if (requestKey.Length < 8 || !OrganizationRoomInput.Identifier(requestKey)) throw new ArgumentException("This creation attempt is invalid.");
        if (name.Length == 0 || !OrganizationRoomInput.Text(name, 80) || !OrganizationRoomInput.Text(description, 500, multiline: true))
            throw new ArgumentException("Use a name of 1–80 characters and a description of at most 500 characters.");
        if (!Enum.IsDefined(join) || !Enum.IsDefined(post)) throw new ArgumentException("Choose valid room policies.");
        OrganizationId = organizationId; Name = name; Description = description; JoinPolicy = join; PostPolicy = post; Listed = listed; RequestKey = requestKey;
        var body = new JsonObject { ["request_key"] = requestKey, ["org_id"] = organizationId, ["name"] = name };
        if (description.Length > 0) body["description"] = description;
        body["join_policy"] = join == RoomJoinPolicy.Members ? "public" : "private";
        body["post_policy"] = post == RoomPostPolicy.Everyone ? "everyone" : "admins";
        body["visibility"] = listed ? "listed" : "unlisted";
        body["history_visible_to_new_members"] = false;
        Encoded = body.ToJsonString();
    }
    // Rebuild only from exactly these bytes (protected custody); any other shape is refused.
    public static OrganizationRoomCreate Restore(string encoded)
    {
        using var document = JsonDocument.Parse(encoded);
        var root = document.RootElement;
        if (root.ValueKind != JsonValueKind.Object || root.EnumerateObject().Select(p => p.Name).Distinct(StringComparer.Ordinal).Count() != root.EnumerateObject().Count())
            throw new ArgumentException("This saved room creation is invalid.");
        string Text(string name) => root.TryGetProperty(name, out var value) && value.ValueKind == JsonValueKind.String ? value.GetString()! : throw new ArgumentException("This saved room creation is invalid.");
        var description = root.TryGetProperty("description", out _) ? Text("description") : "";
        var restored = new OrganizationRoomCreate(Text("org_id"), Text("name"), description,
            Text("join_policy") switch { "public" => RoomJoinPolicy.Members, "private" => RoomJoinPolicy.Invited, _ => throw new ArgumentException("This saved room creation is invalid.") },
            Text("post_policy") switch { "everyone" => RoomPostPolicy.Everyone, "admins" => RoomPostPolicy.Admins, _ => throw new ArgumentException("This saved room creation is invalid.") },
            Text("visibility") switch { "listed" => true, "unlisted" => false, _ => throw new ArgumentException("This saved room creation is invalid.") },
            Text("request_key"));
        if (restored.Encoded != encoded) throw new ArgumentException("This saved room creation is invalid.");
        return restored;
    }
    public override string ToString() => "OrganizationRoomCreate(redacted)";
}

public interface IOrganizationRoomApi
{
    AuthorizationLease? AcceptedAuthorization { get; }
    Task<RoomOrganizations> OrganizationsAsync(string mailbox, CancellationToken ct);
    Task<IReadOnlyList<OrganizationRoom>> RoomsAsync(string mailbox, string organizationId, string search, CancellationToken ct);
    Task<RoomPreview> PreviewRoomAsync(string mailbox, string groupId, CancellationToken ct);
    Task<ConfirmedRoom> RoomAsync(string mailbox, string groupId, CancellationToken ct);
    Task<ConfirmedRoom> CreateRoomAsync(string mailbox, OrganizationRoomCreate payload, CancellationToken ct);
    Task JoinRoomAsync(string mailbox, string groupId, CancellationToken ct);
    Task AcceptRoomInvitationAsync(string mailbox, string groupId, CancellationToken ct);
}

internal static class RoomWire
{
    internal static PostboxException Bad() => new(200, "bad_response");
    internal static JsonElement Object(JsonElement value)
    {
        if (value.ValueKind != JsonValueKind.Object) throw Bad();
        var names = value.EnumerateObject().Select(p => p.Name).ToArray();
        if (names.Distinct(StringComparer.Ordinal).Count() != names.Length) throw Bad();
        return value;
    }
    internal static string? Text(JsonElement value, string name) => value.TryGetProperty(name, out var field)
        ? field.ValueKind switch { JsonValueKind.String => field.GetString(), JsonValueKind.Null => null, _ => throw Bad() } : null;
    internal static string Required(JsonElement value, string name) => Text(value, name) ?? throw Bad();
    internal static int Number(JsonElement value, string name, int fallback) => value.TryGetProperty(name, out var field)
        ? field.ValueKind == JsonValueKind.Number && field.TryGetInt32(out var number) ? number : throw Bad() : fallback;
    internal static bool Flag(JsonElement value, string name) => value.TryGetProperty(name, out var field)
        ? field.ValueKind switch { JsonValueKind.True => true, JsonValueKind.False => false, _ => throw Bad() } : false;
    internal static IReadOnlyList<string> Strings(JsonElement value, string name)
    {
        if (!value.TryGetProperty(name, out var field) || field.ValueKind == JsonValueKind.Null) return [];
        if (field.ValueKind != JsonValueKind.Array || field.GetArrayLength() > 64) throw Bad();
        return field.EnumerateArray().Select(e => e.ValueKind == JsonValueKind.String ? e.GetString()! : throw Bad()).ToArray();
    }
    internal static RoomOrganizations Organizations(JsonElement body)
    {
        Object(body);
        if (!body.TryGetProperty("orgs", out var orgs) || orgs.ValueKind != JsonValueKind.Array || orgs.GetArrayLength() > 1000) throw Bad();
        var list = new List<RoomOrganization>();
        foreach (var raw in orgs.EnumerateArray())
        {
            var org = Object(raw);
            RoomMailbox? mailbox = null;
            if (org.TryGetProperty("my_mailbox", out var box) && box.ValueKind != JsonValueKind.Null)
            { Object(box); mailbox = new(Required(box, "address"), Text(box, "handle")); }
            string? rooms = null;
            if (org.TryGetProperty("settings", out var settings) && settings.ValueKind != JsonValueKind.Null) { Object(settings); rooms = Text(settings, "rooms_creation"); }
            list.Add(new(Required(org, "id"), Required(org, "name"), Required(org, "status"), Required(org, "my_member_id"), Text(org, "namespace"),
                rooms, Strings(org, "my_roles"), mailbox));
        }
        var operated = new List<RoomOperatedMailbox>();
        if (body.TryGetProperty("operated", out var delegated) && delegated.ValueKind != JsonValueKind.Null)
        {
            if (delegated.ValueKind != JsonValueKind.Array || delegated.GetArrayLength() > 10_000) throw Bad();
            foreach (var raw in delegated.EnumerateArray())
            {
                var row = Object(raw);
                operated.Add(new(Required(row, "address"), Required(row, "org_id"), Text(row, "handle"), Strings(row, "capabilities")));
            }
        }
        var result = new RoomOrganizations(list, operated);
        return result.Valid ? result : throw Bad();
    }
    internal static OrganizationRoom Room(JsonElement raw)
    {
        var row = Object(raw);
        var room = new OrganizationRoom(Required(row, "id"), Required(row, "name"), Text(row, "description"), Number(row, "member_count", -1),
            Required(row, "join_policy"), Text(row, "my_state"));
        return room.Valid ? room : throw Bad();
    }
    internal static RoomPreview Preview(JsonElement raw, string id)
    {
        var row = Object(raw);
        var preview = new RoomPreview(Required(row, "id"), Text(row, "name") ?? "Room", Text(row, "description"), Number(row, "member_count", 1),
            Text(row, "join_policy") ?? "private", Text(row, "my_state"), Flag(row, "can_join"));
        return preview.Valid && preview.Id == id ? preview : throw Bad();
    }
    // Default group fields never supply absent authority: organization, state and role must be explicit.
    internal static ConfirmedRoom Confirmed(JsonElement raw, string? id = null, string? organization = null, string? creator = null)
    {
        var group = Object(raw);
        if (!group.TryGetProperty("my", out var my)) throw Bad();
        Object(my);
        var state = Required(my, "state"); var role = Required(my, "role");
        if (state is not ("active" or "keying") || role is not ("owner" or "admin" or "member" or "restricted")) throw Bad();
        var result = new ConfirmedRoom(Required(group, "id"), Required(group, "org_id"), Text(group, "name") ?? "Room", state, role,
            Text(group, "created_by"), Text(group, "activation_state"));
        if (group.TryGetProperty("deleted_at", out var deleted) && deleted.ValueKind != JsonValueKind.Null) throw Bad();
        if (!OrganizationRoomInput.Identifier(result.Id) || !OrganizationRoomInput.Identifier(result.OrganizationId)
            || !OrganizationRoomInput.Text(result.Name, 256) || id is not null && result.Id != id
            || organization is not null && result.OrganizationId != organization
            || creator is not null && (result.CreatedBy != creator || role != "owner")) throw Bad();
        return result;
    }
    internal static string Query(string value) => Uri.EscapeDataString(value);
    internal static string Count(int value) => value.ToString(CultureInfo.InvariantCulture);
}
