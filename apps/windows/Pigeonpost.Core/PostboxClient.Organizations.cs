using System.Text.Json;
using Pigeonpost.Core.Enterprise;

namespace Pigeonpost.Core;

// Organization catalog and room directory over the accepted-login transport and the existing group
// routes. Every call acts as one canonical inbox and keeps its original account/caller constraints.
public sealed partial class PostboxClient : IOrganizationRoomApi
{
    AuthorizationLease? IOrganizationRoomApi.AcceptedAuthorization => AcceptedAuthorization;
    private const int MaxRoomReply = 1024 * 1024;

    private async Task<JsonElement> RoomJsonAsync(HttpMethod method, string path, string? json, string mailbox, CancellationToken ct)
    {
        if (!OrganizationRoomInput.Mailbox(mailbox)) throw new ArgumentException("Choose a valid inbox.", nameof(mailbox));
        using var operation = AuthorizationContext.Capture(AcceptedAuthorization);
        var owner = new RequestAuthorization(tokens);
        using var response = await RequestAsync(method, Scoped(path, mailbox), json, ct, identity: mailbox).ConfigureAwait(false);
        try
        {
            await response.Content.LoadIntoBufferAsync(MaxRoomReply, ct).ConfigureAwait(false);
            var bytes = await response.Content.ReadAsByteArrayAsync(ct).ConfigureAwait(false);
            owner.Check(ct);
            if (bytes.Length == 0) return default;
            using var document = JsonDocument.Parse(bytes);
            var root = RoomWire.Object(document.RootElement).Clone();
            owner.Check(ct);
            return root;
        }
        catch (Exception error) when (error is JsonException or NotSupportedException or HttpRequestException or InvalidOperationException)
        { owner.Check(ct); throw RoomWire.Bad(); }
    }
    private static string RoomPath(string id, string suffix = "") => OrganizationRoomInput.Identifier(id)
        ? "/v1/groups/" + id + suffix : throw new ArgumentException("Invalid room.", nameof(id));

    public async Task<RoomOrganizations> OrganizationsAsync(string mailbox, CancellationToken ct) =>
        RoomWire.Organizations(await RoomJsonAsync(HttpMethod.Get, "/v1/orgs", null, mailbox, ct).ConfigureAwait(false));

    public async Task<IReadOnlyList<OrganizationRoom>> RoomsAsync(string mailbox, string organizationId, string search, CancellationToken ct)
    {
        // Omitting the organization would read the personal public directory instead.
        if (!OrganizationRoomInput.Identifier(organizationId)) throw new ArgumentException("Choose a valid organization.", nameof(organizationId));
        if (!OrganizationRoomInput.Text(search, 80)) throw new ArgumentException("Use at most 80 characters to search.", nameof(search));
        var body = await RoomJsonAsync(HttpMethod.Get, "/v1/rooms?org_id=" + organizationId + "&q=" + RoomWire.Query(search), null, mailbox, ct).ConfigureAwait(false);
        if (!body.TryGetProperty("rooms", out var rows) || rows.ValueKind != JsonValueKind.Array || rows.GetArrayLength() > 50) throw RoomWire.Bad();
        var rooms = rows.EnumerateArray().Select(RoomWire.Room).ToArray();
        return rooms.Select(r => r.Id).Distinct(StringComparer.Ordinal).Count() == rooms.Length ? rooms : throw RoomWire.Bad();
    }

    public async Task<RoomPreview> PreviewRoomAsync(string mailbox, string groupId, CancellationToken ct)
    {
        RoomPath(groupId);
        return RoomWire.Preview(await RoomJsonAsync(HttpMethod.Post, "/v1/groups/resolve",
            JsonSerializer.Serialize(new { group_id = groupId }), mailbox, ct).ConfigureAwait(false), groupId);
    }

    public async Task<ConfirmedRoom> RoomAsync(string mailbox, string groupId, CancellationToken ct)
    {
        var value = await RoomJsonAsync(HttpMethod.Get, RoomPath(groupId), null, mailbox, ct).ConfigureAwait(false);
        // The deployed single-group route returns the object itself; an envelope is also accepted.
        var raw = value.ValueKind == JsonValueKind.Object && value.TryGetProperty("group", out var group) ? group : value;
        return RoomWire.Confirmed(raw, id: groupId);
    }

    public async Task<ConfirmedRoom> CreateRoomAsync(string mailbox, OrganizationRoomCreate payload, CancellationToken ct)
    {
        var value = await RoomJsonAsync(HttpMethod.Post, "/v1/groups", payload.Encoded, mailbox, ct).ConfigureAwait(false);
        if (value.ValueKind != JsonValueKind.Object || !value.TryGetProperty("group", out var group)) throw RoomWire.Bad();
        return RoomWire.Confirmed(group, organization: payload.OrganizationId, creator: mailbox);
    }

    public async Task JoinRoomAsync(string mailbox, string groupId, CancellationToken ct) =>
        await RoomJsonAsync(HttpMethod.Post, RoomPath(groupId, "/join"), "{}", mailbox, ct).ConfigureAwait(false);

    public async Task AcceptRoomInvitationAsync(string mailbox, string groupId, CancellationToken ct) =>
        await RoomJsonAsync(HttpMethod.Post, RoomPath(groupId, "/invitation"), "{\"accept\":true}", mailbox, ct).ConfigureAwait(false);
}
