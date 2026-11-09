using System.Text.Json;

namespace Pigeonpost.Core.Enterprise;

// SDS §5.4: members experience the organization inside the app. The selected org inbox shows a badge
// ("Acme Inc. · Member"), and composing from it suggests organization addresses from
// GET /v1/orgs/{id}/directory?q= (active members and agents only; the server checks membership).
public sealed record DirectoryEntry(string Handle, string Address, string Kind, string? Name)
{
    public bool Valid => OrganizationRoomInput.Text(Handle, 256) && PostAddress.IsValid(Handle) && OrganizationRoomInput.Mailbox(Address)
        && Kind is "member" or "agent" && (Name is null || OrganizationRoomInput.Text(Name, 256));
    // "Ayşe Yılmaz · /acme/ayse" for people, "Deploy bot · agent · /acme/deploy" for agents.
    public string Label => string.Join(" · ", new[] { Name, Kind == "agent" ? "agent" : null, Handle }.Where(p => !string.IsNullOrWhiteSpace(p)));
    public override string ToString() => "DirectoryEntry(redacted)";
}

public static class OrganizationBadge
{
    // The strongest role wins; plain members (no admin role) read "Member".
    public static string Role(IReadOnlyList<string> roles) =>
        roles.Contains("owner") ? "Owner" : roles.Contains("admin") ? "Admin" : roles.Contains("people_admin") ? "People admin"
        : roles.Contains("agent_admin") ? "Agent admin" : roles.Contains("billing_admin") ? "Billing admin" : "Member";

    public static string Label(RoomOrganization org) => org.Status switch
    {
        "read_only" => $"{org.Name} · {Role(org.Roles)} · read-only",
        "suspended" => $"{org.Name} · suspended",
        _ => $"{org.Name} · {Role(org.Roles)}",
    };

    // One or two letters for the badge until organization logos can be uploaded (R-25 / audit P2-20).
    public static string Initials(string name) => ConversationPresentation.Initials(name);

    // The organization whose own member inbox is this mailbox, if any.
    public static RoomOrganization? For(RoomOrganizations catalog, string? mailbox) =>
        mailbox is null ? null : catalog.OwnedFor(mailbox).FirstOrDefault();

    // A directory query worth sending: 2–80 characters once the leading "/" and the org namespace are dropped.
    public static string? Query(string input, string? orgNamespace)
    {
        var q = (input ?? "").Trim().TrimStart('/');
        if (orgNamespace is { Length: > 0 } ns && q.StartsWith(ns + "/", StringComparison.OrdinalIgnoreCase)) q = q[(ns.Length + 1)..];
        q = q.Replace("%", "").Replace("_", "");
        return q.Length is >= 2 and <= 80 ? q : null;
    }
}

public sealed class OrganizationDirectoryClient(PostboxClient postbox)
{
    public async Task<IReadOnlyList<DirectoryEntry>> SearchAsync(string mailbox, string organizationId, string query, CancellationToken ct)
    {
        if (!OrganizationRoomInput.Identifier(organizationId)) throw new ArgumentException("Choose a valid organization.");
        if (query.Length is < 1 or > 80) throw new ArgumentException("Type part of a name or handle.");
        var body = await postbox.OrganizationDirectoryJsonAsync(mailbox, organizationId, query, ct).ConfigureAwait(false);
        if (body.ValueKind != JsonValueKind.Object || !body.TryGetProperty("entries", out var entries) || entries.ValueKind != JsonValueKind.Array
            || entries.GetArrayLength() > 50) throw RoomWire.Bad();
        var parsed = entries.EnumerateArray().Select(e =>
        {
            if (e.ValueKind != JsonValueKind.Object) throw RoomWire.Bad();
            var entry = new DirectoryEntry(RoomWire.Required(e, "handle"), RoomWire.Required(e, "address"), RoomWire.Required(e, "kind"), RoomWire.Text(e, "name"));
            return entry.Valid ? entry : throw RoomWire.Bad();
        }).ToArray();
        return parsed.Where(e => e.Address != mailbox).DistinctBy(e => e.Address).ToArray();
    }
}
