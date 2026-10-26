using System.Net;
using System.Security.Cryptography;
using System.Text;
using System.Text.Json;
using System.Text.Json.Nodes;
using Pigeonpost.Core.Groups;
using GC = Pigeonpost.GroupCrypto.GroupCrypto;

namespace Pigeonpost.Core.Preview;

// A stand-in postbox for the Phase 1b and Phase 3 routes, compiled only into the core tests and
// the disposable UI-test app (never Store packages). It keeps the server's rules that the clients
// depend on — device attestation by the mailbox key, pending adds and removals, the first commit
// for an epoch winning, epoch-bound posts, welcomes addressed by key-package reference, posting
// policy and membership — so the real MLS library runs end to end against it. It never decrypts
// anything: like the real postbox it sees only ciphertext.
public sealed class PreviewPostbox : HttpMessageHandler
{
    private readonly object sync = new();
    private long clock = 1_789_113_600;
    private int counter;

    public HashSet<string> Features { get; } = new(StringComparer.Ordinal)
    {
        ServerFeatures.MessageFiltering, ServerFeatures.CallAdmission, ServerFeatures.CallHistory,
        ServerFeatures.Avatars, ServerFeatures.Groups, ServerFeatures.Disappearing,
    };
    public Func<long>? Clock { get; set; }
    private long Now => Clock?.Invoke() ?? clock;
    public void Advance(long seconds) => clock += seconds;
    public List<string> Requests { get; } = [];
    public List<(string Mailbox, string Kind, string Id)> Reports { get; } = [];

    // ---- mailboxes ---------------------------------------------------------------------------

    private sealed class Box(string address, string? handle, byte[] seed, byte[] publicKey)
    {
        public string Address { get; } = address;
        public string? Handle { get; } = handle;
        public byte[] Seed { get; } = seed;
        public byte[] PublicKey { get; } = publicKey;
        public JsonObject Policy { get; set; } = new()
        {
            ["unknown_calls"] = "ring", ["unnamed_calls"] = "silence", ["unnamed_messages"] = "show",
            ["unknown_messages"] = "show", ["low_reputation"] = "filter", ["group_members_known"] = true, ["call_history_days"] = 180,
        };
        public List<InboxMessage> Filtered { get; } = [];
        public List<Contact> Blocked { get; } = [];
        public List<CallLogEntry> Calls { get; } = [];
        public long SeenUpTo { get; set; }
        public (AvatarInfo Info, byte[] Image)? Avatar { get; set; }
        public Dictionary<string, int?> Timers { get; } = new(StringComparer.Ordinal);
    }

    private readonly Dictionary<string, Box> boxes = new(StringComparer.Ordinal);

    public static string AddressForKey(byte[] publicKey)
    {
        const string b32 = "0123456789abcdefghjkmnpqrstvwxyz"; // Crockford, as in the crate
        var digest = SHA256.HashData(publicKey);
        var sb = new StringBuilder("/k/");
        int buffer = 0, bits = 0;
        foreach (var b in digest[..16])
        {
            buffer = (buffer << 8) | b;
            bits += 8;
            while (bits >= 5) { bits -= 5; sb.Append(b32[(buffer >> bits) & 31]); }
        }
        sb.Append(b32[(buffer << (5 - bits)) & 31]);
        return sb.ToString();
    }

    public static byte[] Seed(byte fill) => Enumerable.Repeat(fill, 32).ToArray();
    public static string AddressFor(byte[] seed) => AddressForKey(Ed25519Public.FromSeed(seed));

    public string AddMailbox(byte[] seed, string? handle = null)
    {
        var key = Ed25519Public.FromSeed(seed);
        var address = AddressForKey(key);
        lock (sync) boxes[address] = new Box(address, handle, seed, key);
        return address;
    }

    private Box BoxFor(string? key) => key is not null && (boxes.TryGetValue(key, out var b)
        || boxes.Values.FirstOrDefault(x => x.Handle == key || x.Handle is { } h && PostAddress.Canonical(h) == PostAddress.Canonical(key)) is { } found && (b = found) is not null)
        ? b : throw new Fail(403, "wrong_identity");

    private string? Resolve(string peer) => boxes.TryGetValue(peer, out var b) ? b.Address
        : boxes.Values.FirstOrDefault(x => x.Handle is { } h && PostAddress.Canonical(h) == PostAddress.Canonical(peer))?.Address;

    // ---- seeding helpers (tests and the UI preview) ---------------------------------------

    public void AddFiltered(string mailbox, InboxMessage message) { lock (sync) boxes[mailbox].Filtered.Add(message); }
    public void AddCall(string mailbox, CallLogEntry entry) { lock (sync) boxes[mailbox].Calls.Add(entry); }
    public IReadOnlyList<Contact> BlockedOf(string mailbox) { lock (sync) return boxes[mailbox].Blocked.ToArray(); }
    public JsonObject PolicyOf(string mailbox) { lock (sync) return (JsonObject)boxes[mailbox].Policy.DeepClone(); }
    public int? TimerOf(string mailbox, string thread) { lock (sync) return boxes[mailbox].Timers.GetValueOrDefault(thread); }
    public bool HasAvatar(string mailbox) { lock (sync) return boxes[mailbox].Avatar is not null; }
    public string? AvatarVisibility(string mailbox) { lock (sync) return boxes[mailbox].Avatar?.Info.Visibility; }

    // ---- groups state ------------------------------------------------------------------------

    private sealed class Device
    {
        public required string Mailbox { get; init; }
        public required string Id { get; init; }
        public required byte[] SignatureKey { get; init; }
        public bool Revoked { get; set; }
        public List<(string Package, string Ref)> Packages { get; } = [];
        public (string Package, string Ref)? LastResort { get; set; }
        public List<(string Id, string Group, long Seq, long Epoch, string Welcome)> Welcomes { get; } = [];
    }

    private sealed class Member
    {
        public required string Address { get; init; }
        public string Role { get; set; } = "member";
        public string State { get; set; } = "invited";
        public long Revision { get; set; } = 1;
        public string? InvitedBy { get; set; }
        public long? JoinedAt { get; set; }
        public long FirstSeq { get; set; } = long.MaxValue;
        public long ReadCursor { get; set; }
    }

    private sealed class Grp
    {
        public required string Id { get; init; }
        public string Name { get; set; } = "";
        public string? Description { get; set; }
        public string JoinPolicy { get; set; } = "private";
        public string PostPolicy { get; set; } = "everyone";
        public string Visibility { get; set; } = "unlisted";
        public bool MembersCanInvite { get; set; }
        public bool MembersCanChangeTimer { get; set; }
        public int? EphemeralSecs { get; set; }
        public long Revision { get; set; } = 1;
        public long Epoch { get; set; }
        public string Activation { get; set; } = "provisioning";
        public required string CreatedBy { get; init; }
        public long CreatedAt { get; init; }
        public long? DeletedAt { get; set; }
        public Dictionary<string, Member> Members { get; } = new(StringComparer.Ordinal);
        public HashSet<(string Mailbox, string Device)> Leaves { get; } = [];
        public List<JsonObject> Items { get; } = [];
        public Dictionary<string, byte[]> Attachments { get; } = new(StringComparer.Ordinal);
        public Dictionary<string, (string Mailbox, string Device, string By)> Claims { get; } = new(StringComparer.Ordinal);
        public Dictionary<string, long> Cmids { get; } = new(StringComparer.Ordinal);
        public long Seq { get; set; }
    }

    private readonly Dictionary<(string, string), Device> devices = [];
    private readonly Dictionary<string, Grp> groups = new(StringComparer.Ordinal);
    public int GroupCount { get { lock (sync) return groups.Count; } }
    public long EpochOf(string gid) { lock (sync) return groups[gid].Epoch; }
    public IReadOnlyList<string> CiphertextsOf(string gid) { lock (sync) return groups[gid].Items.Where(i => (string?)i["type"] == "application").Select(i => (string?)i["ciphertext"] ?? "").ToArray(); }
    public IReadOnlyList<byte[]> AttachmentBlobsOf(string gid) { lock (sync) return groups[gid].Attachments.Values.ToArray(); }
    public void RevokeDevice(string mailbox, string device) { lock (sync) devices[(mailbox, device)].Revoked = true; }
    public string? DeviceIdOf(string mailbox) { lock (sync) return devices.Values.FirstOrDefault(d => d.Mailbox == mailbox && !d.Revoked)?.Id; }

    // ---- HTTP --------------------------------------------------------------------------------

    private sealed class Fail(int status, string code, JsonObject? extra = null) : Exception(code)
    {
        public int Status { get; } = status;
        public string Code { get; } = code;
        public JsonObject? Extra { get; } = extra;
    }

    protected override async Task<HttpResponseMessage> SendAsync(HttpRequestMessage request, CancellationToken cancellationToken)
    {
        var path = request.RequestUri!.AbsolutePath;
        var query = System.Web.HttpUtility.ParseQueryString(request.RequestUri.Query);
        var identity = request.Headers.TryGetValues("x-pigeonpost-identity", out var ids) ? ids.First() : query["identity"];
        var device = request.Headers.TryGetValues("x-pigeonpost-device", out var devs) ? devs.First() : null;
        byte[] raw = request.Content is null ? [] : await request.Content.ReadAsByteArrayAsync(cancellationToken).ConfigureAwait(false);
        JsonNode? body = null;
        if (raw.Length > 0 && request.Content?.Headers.ContentType?.MediaType == "application/json") body = JsonNode.Parse(raw);
        lock (sync)
        {
            Requests.Add(request.Method + " " + path);
            try
            {
                var result = Route(request.Method.Method, path, query, identity, device, body, raw);
                return result switch
                {
                    byte[] bytes => new HttpResponseMessage(HttpStatusCode.OK) { Content = new ByteArrayContent(bytes) },
                    (int status, JsonNode node) => Json(status, node),
                    JsonNode node => Json(200, node),
                    _ => Json(200, new JsonObject()),
                };
            }
            catch (Fail f)
            {
                var error = f.Extra ?? new JsonObject();
                error["error"] = f.Code;
                error["message"] = f.Code;
                return Json(f.Status, error);
            }
        }
    }

    private static HttpResponseMessage Json(int status, JsonNode node) => new((HttpStatusCode)status)
    {
        Content = new StringContent(node.ToJsonString(), Encoding.UTF8, "application/json")
    };

    private object? Route(string method, string path, System.Collections.Specialized.NameValueCollection query, string? identity, string? device, JsonNode? body, byte[] raw)
    {
        if (path == "/v1/capabilities") return new JsonObject { ["features"] = new JsonArray(Features.Select(f => (JsonNode)f).ToArray()) };
        if (path.StartsWith("/v1/avatar-blobs/", StringComparison.Ordinal))
        {
            var box = boxes.Values.FirstOrDefault(b => b.Avatar is { } a && path.StartsWith($"/v1/avatar-blobs/{a.Info.Id}/{a.Info.Version}/", StringComparison.Ordinal));
            return box?.Avatar?.Image ?? throw new Fail(404, "not_found");
        }
        if (path == "/v1/avatars")
        {
            var map = new JsonObject();
            foreach (var key in (query["addresses"] ?? "").Split(',', StringSplitOptions.RemoveEmptyEntries))
            {
                var address = Resolve(key);
                var avatar = address is null ? null : boxes[address].Avatar;
                var visible = avatar is { } a && a.Info.Visibility != "nobody";
                map[key] = visible ? PublicAvatar(avatar!.Value.Info) : null;
            }
            return new JsonObject { ["avatars"] = map };
        }
        var me = BoxFor(identity);
        switch (method, path)
        {
            case ("GET", "/v1/whoami"): return new JsonObject { ["address"] = me.Address, ["handle"] = me.Handle };
            // ---- Phase 1b ----------------------------------------------------------------
            case ("GET", "/v1/policy"): return me.Policy.DeepClone();
            case ("PUT", "/v1/policy"):
                foreach (var (k, v) in body!.AsObject())
                    if (k != "identity")
                    {
                        if (k == "call_history_days" && v?.GetValue<int>() is not (0 or 30 or 180 or 365)) throw new Fail(400, "invalid_policy");
                        me.Policy[k] = v?.DeepClone();
                    }
                return me.Policy.DeepClone();
            case ("GET", "/v1/inbox") when query["filtered"] == "1":
                return new JsonObject { ["messages"] = JsonSerializer.SerializeToNode(me.Filtered, PostboxJson.Options) };
            case ("POST", "/v1/filtered/accept"):
            {
                var peer = (string)body!["peer"]!;
                var moved = me.Filtered.RemoveAll(m => (m.SenderHandle ?? m.From) == peer || m.From == peer);
                return new JsonObject { ["accepted"] = true, ["moved"] = moved };
            }
            case ("GET", "/v1/contacts") when query["admission"] == "block":
                return new JsonObject { ["contacts"] = JsonSerializer.SerializeToNode(me.Blocked, PostboxJson.Options) };
            case ("PUT", "/v1/contacts"):
            {
                var peer = (string)body!["peer"]!;
                me.Blocked.RemoveAll(c => c.Peer == peer);
                if ((string?)body["admission"] == "block") me.Blocked.Add(new Contact(peer, (string?)body["alias"], "block", "review", []));
                return new JsonObject { ["ok"] = true };
            }
            case ("DELETE", "/v1/contacts"):
                me.Blocked.RemoveAll(c => c.Peer == query["peer"]);
                return new JsonObject { ["removed"] = true };
            case ("POST", "/v1/report-spam"):
                Reports.Add((me.Address, body!["call_id"] is not null ? "call" : "message", (string?)body["call_id"] ?? (string?)body["message_id"] ?? ""));
                return new JsonObject { ["reported"] = true };
            case ("POST", "/v1/report-avatar"):
                Reports.Add((me.Address, "avatar", (string?)body!["address"] ?? ""));
                return new JsonObject { ["reported"] = true };
            case ("GET", "/v1/calls/log"):
            {
                var filter = query["filter"] ?? "all";
                var before = long.TryParse(query["before"], out var b) ? b : long.MaxValue;
                var limit = int.TryParse(query["limit"], out var l) ? l : 50;
                var rows = me.Calls.Where(c => c.StartedAt < before && (filter == "filtered" ? c.IsFiltered : !c.IsFiltered && (filter != "missed" || c.Outcome == "missed")))
                    .OrderByDescending(c => c.StartedAt).ToArray();
                var page = rows.Take(limit).ToArray();
                var unseen = me.Calls.Count(c => c.IsIncoming && c.Outcome == "missed" && c.StartedAt > me.SeenUpTo);
                return new JsonObject
                {
                    ["entries"] = JsonSerializer.SerializeToNode(page, PostboxJson.Options), ["unseen_missed"] = unseen,
                    ["next_before"] = rows.Length > limit ? page[^1].StartedAt : null,
                };
            }
            case ("POST", "/v1/calls/log/seen"):
                me.SeenUpTo = Math.Max(me.SeenUpTo, (long)body!["up_to"]!);
                return new JsonObject { ["unseen_missed"] = me.Calls.Count(c => c.IsIncoming && c.Outcome == "missed" && c.StartedAt > me.SeenUpTo) };
            case ("DELETE", "/v1/calls/log"):
                me.Calls.Clear();
                return new JsonObject { ["deleted"] = 1 };
            case ("DELETE", _) when path.StartsWith("/v1/calls/log/", StringComparison.Ordinal):
                return new JsonObject { ["deleted"] = me.Calls.RemoveAll(c => c.Id == Uri.UnescapeDataString(path[14..])) > 0 };
            case ("GET", "/v1/profile/avatar"):
                return new JsonObject { ["avatar"] = me.Avatar is { } own ? OwnAvatar(own.Info) : null };
            case ("PUT", "/v1/profile/avatar"):
            {
                if (raw.Length == 0) throw new Fail(422, "bad_image");
                var visibility = query["visibility"] ?? "known";
                var version = (me.Avatar?.Info.Version ?? 0) + 1;
                var info = new AvatarInfo("av_" + ++counter, version, Convert.ToHexString(SHA256.HashData(raw)).ToLowerInvariant(), Urls("av_" + counter, version), visibility, Now);
                me.Avatar = (info, raw);
                return new JsonObject { ["avatar"] = OwnAvatar(info) };
            }
            case ("PATCH", "/v1/profile/avatar"):
            {
                if (me.Avatar is not { } current) throw new Fail(404, "not_found");
                var version = current.Info.Version + 1;
                var info = current.Info with { Version = version, Visibility = (string?)body!["visibility"], Urls = Urls(current.Info.Id, version) };
                me.Avatar = (info, current.Image);
                return new JsonObject { ["avatar"] = OwnAvatar(info) };
            }
            case ("DELETE", "/v1/profile/avatar"):
                me.Avatar = null;
                return new JsonObject { ["removed"] = true };
            case ("PATCH", _) when path.StartsWith("/v1/threads/", StringComparison.Ordinal):
            {
                var secs = body!["ephemeral_secs"] is { } s ? s.GetValue<int?>() : null;
                if (secs is not null && !Timers.Choices.Contains(secs)) throw new Fail(400, "bad_request");
                me.Timers[Uri.UnescapeDataString(path[12..])] = secs;
                return new JsonObject { ["ephemeral_secs"] = secs };
            }
        }
        if (path.StartsWith("/v1/mls/", StringComparison.Ordinal) || path.StartsWith("/v1/groups", StringComparison.Ordinal))
        {
            if (!Features.Contains(ServerFeatures.Groups)) throw new Fail(404, "feature_disabled");
            return GroupRoute(method, path, me, device, body, raw, query);
        }
        throw new Fail(404, "not_found");
    }

    private static AvatarUrls Urls(string id, int version) => new()
    {
        Small = $"/v1/avatar-blobs/{id}/{version}/64.webp", Medium = $"/v1/avatar-blobs/{id}/{version}/128.webp",
        Large = $"/v1/avatar-blobs/{id}/{version}/256.webp", Largest = $"/v1/avatar-blobs/{id}/{version}/512.webp",
        Jpeg = $"/v1/avatar-blobs/{id}/{version}/256.jpg",
    };
    private static JsonNode? OwnAvatar(AvatarInfo info) => JsonSerializer.SerializeToNode(info, PostboxJson.Options);
    private static JsonNode? PublicAvatar(AvatarInfo info) => JsonSerializer.SerializeToNode(info with { Visibility = null, CreatedAt = null }, PostboxJson.Options);

    // ---- groups routes -------------------------------------------------------------------

    private Device DeviceOf(Box me, string? device) =>
        device is not null && devices.TryGetValue((me.Address, device), out var d)
            ? d.Revoked ? throw new Fail(403, "device_revoked") : d
            : throw new Fail(403, "unknown_device");

    private Grp GroupOf(string id, Box me)
    {
        if (!groups.TryGetValue(id, out var g) || !g.Members.ContainsKey(me.Address))
            throw new Fail(404, "not_found");
        if (g.DeletedAt is not null) throw new Fail(410, "group_deleted");
        return g;
    }

    private static string Hex(byte[] bytes) => Convert.ToHexString(bytes).ToLowerInvariant();
    private string NewId(string prefix) => prefix + "_" + (++counter).ToString("d6");

    private object? GroupRoute(string method, string path, Box me, string? deviceId, JsonNode? body, byte[] raw, System.Collections.Specialized.NameValueCollection query)
    {
        var parts = path.Split('/', StringSplitOptions.RemoveEmptyEntries); // v1, groups|mls, ...
        if (parts[1] == "mls")
        {
            switch (method, parts.Length > 2 ? parts[2] : "", parts.Length)
            {
                case ("POST", "devices", 3):
                {
                    var id = (string)body!["device_id"]!;
                    if (devices.TryGetValue((me.Address, id), out var existing) && !existing.Revoked) throw new Fail(409, "device_exists");
                    var key = Convert.FromBase64String((string)body["signature_key"]!);
                    devices[(me.Address, id)] = new Device { Mailbox = me.Address, Id = id, SignatureKey = key };
                    var payload = AttestationPayload(me.Address, id, key);
                    var attestation = GC.SignAttestationWithSeed(me.Seed, payload);
                    return (201, new JsonObject
                    {
                        ["device_id"] = id, ["fingerprint"] = "", ["fingerprint_digits"] = null,
                        ["attestation"] = Convert.ToBase64String(attestation), ["mailbox_public_key"] = Convert.ToBase64String(me.PublicKey), ["created_at"] = Now,
                    });
                }
                case ("GET", "devices", 3):
                {
                    var of = query["mailbox"] ?? me.Address;
                    return new JsonObject
                    {
                        ["mailbox"] = of,
                        ["devices"] = new JsonArray(devices.Values.Where(d => d.Mailbox == of && !d.Revoked).Select(d => (JsonNode)new JsonObject
                        {
                            ["device_id"] = d.Id, ["signature_key"] = Convert.ToBase64String(d.SignatureKey), ["created_at"] = Now,
                        }).ToArray()),
                    };
                }
                case ("DELETE", "devices", 4):
                    DeviceOf(me, parts[3]).Revoked = true;
                    return new JsonObject { ["revoked"] = true };
                case ("GET", "key-packages", 4):
                {
                    var d = DeviceOf(me, deviceId);
                    return new JsonObject { ["unclaimed"] = d.Packages.Count, ["last_resort"] = d.LastResort is not null };
                }
                case ("POST", "key-packages", 3):
                {
                    var d = DeviceOf(me, deviceId);
                    foreach (var p in body!["key_packages"]!.AsArray())
                    {
                        var package = (string)p!;
                        var info = GC.InspectKeyPackage(Convert.FromBase64String(package));
                        if (info.Mailbox != me.Address || info.DeviceId != d.Id) throw new Fail(400, "bad_key_package");
                        d.Packages.Add((package, Hex(info.KeyPackageRef)));
                    }
                    if (body["last_resort"] is { } lr)
                    {
                        var info = GC.InspectKeyPackage(Convert.FromBase64String((string)lr!));
                        d.LastResort = ((string)lr!, Hex(info.KeyPackageRef));
                    }
                    return new JsonObject { ["stored"] = d.Packages.Count, ["unclaimed"] = d.Packages.Count, ["last_resort"] = d.LastResort is not null };
                }
                case ("GET", "welcomes", 3):
                {
                    var d = DeviceOf(me, deviceId);
                    return new JsonObject
                    {
                        ["welcomes"] = new JsonArray(d.Welcomes.Select(w => (JsonNode)new JsonObject
                        {
                            ["welcome_id"] = w.Id, ["group_id"] = w.Group, ["epoch"] = w.Epoch, ["seq"] = w.Seq, ["welcome"] = w.Welcome,
                        }).ToArray()),
                    };
                }
                case ("POST", "welcomes", 5):
                {
                    var d = DeviceOf(me, deviceId);
                    d.Welcomes.RemoveAll(w => w.Id == parts[3]);
                    return new JsonObject { ["ok"] = true };
                }
            }
            throw new Fail(404, "not_found");
        }
        if (parts.Length == 2)
        {
            if (method == "POST")
            {
                var g = new Grp { Id = "grp_" + GroupApiIdSafe(), CreatedBy = me.Address, CreatedAt = Now };
                g.Name = (string)body!["name"]!;
                g.Description = (string?)body["description"];
                g.JoinPolicy = (string?)body["join_policy"] ?? "private";
                g.PostPolicy = (string?)body["post_policy"] ?? "everyone";
                g.MembersCanInvite = (bool?)body["members_can_invite"] ?? false;
                g.MembersCanChangeTimer = (bool?)body["members_can_change_timer"] ?? false;
                g.EphemeralSecs = (int?)body["ephemeral_secs"];
                g.Members[me.Address] = new Member { Address = me.Address, Role = "owner", State = "keying" };
                var results = new JsonArray();
                foreach (var target in body["invite"]?.AsArray() ?? [])
                {
                    var address = Resolve((string)target!);
                    if (address is null) { results.Add(new JsonObject { ["address"] = (string)target!, ["status"] = "unresolved" }); continue; }
                    g.Members[address] = new Member { Address = address, State = "invited", InvitedBy = me.Address };
                    results.Add(new JsonObject { ["address"] = address, ["status"] = "invited" });
                }
                groups[g.Id] = g;
                return (201, new JsonObject { ["group"] = GroupJson(g, me), ["invitations"] = results });
            }
            return new JsonObject
            {
                ["groups"] = new JsonArray(groups.Values.Where(g => g.Members.TryGetValue(me.Address, out var m) && m.State is "active" or "keying" or "invited"
                        && !(m.State == "invited" && g.Activation == "provisioning"))
                    .OrderByDescending(g => g.Items.Count == 0 ? g.CreatedAt : (long)g.Items[^1]["created_at"]!).Select(g => GroupJson(g, me)).ToArray()),
            };
        }
        var grp = GroupOf(parts[2], me);
        var mine = grp.Members[me.Address];
        var action = parts.Length > 3 ? parts[3] : "";
        switch (method, action)
        {
            case ("GET", ""): return GroupJson(grp, me);
            case ("PATCH", ""):
            {
                var timerOnly = body!.AsObject().All(p => p.Key is "expected_revision" or "ephemeral_secs" or "identity");
                if (!(mine.Role is "owner" or "admin" || timerOnly && grp.MembersCanChangeTimer)) throw new Fail(403, "not_admin");
                if ((long)body["expected_revision"]! != grp.Revision) throw new Fail(409, "revision_conflict", new JsonObject { ["current_revision"] = grp.Revision });
                var changed = new JsonArray();
                foreach (var (k, v) in body.AsObject())
                {
                    switch (k)
                    {
                        case "name": grp.Name = (string)v!; break;
                        case "description": grp.Description = (string?)v; break;
                        case "post_policy": grp.PostPolicy = (string)v!; break;
                        case "join_policy": grp.JoinPolicy = (string)v!; break;
                        case "members_can_invite": grp.MembersCanInvite = (bool)v!; break;
                        case "members_can_change_timer": grp.MembersCanChangeTimer = (bool)v!; break;
                        case "ephemeral_secs":
                            var secs = v?.GetValue<int?>();
                            if (secs is not null && !Timers.Choices.Contains(secs)) throw new Fail(400, "bad_request");
                            grp.EphemeralSecs = secs; break;
                        default: continue;
                    }
                    changed.Add(k);
                }
                grp.Revision++;
                Notice(grp, changed.Any(c => (string?)c == "ephemeral_secs") ? "timer_changed" : "settings_changed", me.Address,
                    new JsonObject { ["changed"] = changed, ["ephemeral_secs"] = grp.EphemeralSecs, ["post_policy"] = grp.PostPolicy });
                return new JsonObject { ["group"] = GroupJson(grp, me) };
            }
            case ("DELETE", ""):
                if (mine.Role != "owner") throw new Fail(403, "owner_only");
                grp.DeletedAt = Now;
                return new JsonObject { ["deleted"] = true };
            case ("GET", "members"):
                return new JsonObject
                {
                    ["members"] = new JsonArray(grp.Members.Values.Where(m => m.State is "active" or "keying" || mine.Role is "owner" or "admin" && m.State is "invited" or "banned")
                        .Select(m => (JsonNode)new JsonObject
                        {
                            ["address"] = m.Address, ["handle"] = boxes.GetValueOrDefault(m.Address)?.Handle, ["role"] = m.Role, ["state"] = m.State,
                            ["invited_by"] = m.InvitedBy, ["joined_at"] = m.JoinedAt, ["revision"] = m.Revision,
                        }).ToArray()),
                };
            case ("POST", "members"):
            {
                if (!(mine.Role is "owner" or "admin" || grp.MembersCanInvite)) throw new Fail(403, "not_admin");
                var results = new JsonArray();
                foreach (var target in body!["invite"]!.AsArray())
                {
                    var address = Resolve((string)target!);
                    if (address is null) { results.Add(new JsonObject { ["address"] = (string)target!, ["status"] = "unresolved" }); continue; }
                    if (grp.Members.TryGetValue(address, out var existing) && existing.State is "active" or "keying") { results.Add(new JsonObject { ["address"] = address, ["status"] = "already_member" }); continue; }
                    grp.Members[address] = new Member { Address = address, State = "invited", InvitedBy = me.Address };
                    Notice(grp, "member_invited", me.Address, new JsonObject { ["address"] = address });
                    results.Add(new JsonObject { ["address"] = address, ["status"] = "invited" });
                }
                return new JsonObject { ["results"] = results };
            }
            case ("PATCH", "members"):
            {
                var target = Uri.UnescapeDataString(parts[4]);
                if (!grp.Members.TryGetValue(target, out var row)) throw new Fail(404, "not_found");
                if ((long)body!["expected_revision"]! != row.Revision) throw new Fail(409, "revision_conflict", new JsonObject { ["current_revision"] = row.Revision });
                if (!(mine.Role is "owner" or "admin")) throw new Fail(403, "not_admin");
                if ((string?)body["role"] is { } role) { row.Role = role; Notice(grp, "role_changed", me.Address, new JsonObject { ["address"] = target, ["role"] = role }); }
                if ((string?)body["state"] is { } state) { row.State = state; Notice(grp, state == "banned" ? "member_banned" : "member_removed", me.Address, new JsonObject { ["address"] = target }); }
                row.Revision++;
                grp.Revision++;
                return new JsonObject { ["address"] = target };
            }
            case ("POST", "invitation"):
                if (mine.State != "invited") throw new Fail(409, "already_member");
                if ((bool)body!["accept"]!) mine.State = "keying"; else grp.Members.Remove(me.Address);
                return new JsonObject { ["ok"] = true };
            case ("POST", "leave"):
                mine.State = "left";
                Notice(grp, "member_left", me.Address, new JsonObject { ["address"] = me.Address });
                return new JsonObject { ["left"] = true };
            case ("POST", "read"):
                mine.ReadCursor = Math.Max(mine.ReadCursor, (long)body!["seq"]!);
                return new JsonObject { ["ok"] = true };
            case ("POST", "link"):
                return new JsonObject { ["link_token"] = (bool)body!["enabled"]! ? "tok" + ++counter : null, ["url"] = null };
            case ("GET", "crypto"):
            {
                var d = DeviceOf(me, deviceId);
                return new JsonObject
                {
                    ["epoch"] = grp.Epoch, ["revision"] = grp.Revision, ["activation_state"] = grp.Activation,
                    ["in_group"] = grp.Leaves.Contains((me.Address, d.Id)),
                    ["pending"] = new JsonArray(Pending(grp).Select(p => (JsonNode)new JsonObject { ["kind"] = p.Kind, ["mailbox"] = p.Mailbox, ["device_id"] = p.Device }).ToArray()),
                    ["lagging_devices"] = new JsonArray(),
                };
            }
            case ("POST", "key-packages"):
            {
                var d = DeviceOf(me, deviceId);
                if (mine.State != "active" && !(grp.Activation == "provisioning" && mine.Role == "owner")) throw new Fail(403, "not_member");
                var pending = Pending(grp).Where(p => p.Kind == "add").ToArray();
                var claims = new JsonArray();
                var exhausted = new JsonArray();
                foreach (var mailbox in body!["mailboxes"]!.AsArray().Select(m => (string)m!))
                    foreach (var p in pending.Where(p => p.Mailbox == mailbox))
                    {
                        var target = devices[(p.Mailbox, p.Device)];
                        (string Package, string Ref)? kp = target.Packages.Count > 0 ? target.Packages[0] : target.LastResort;
                        if (kp is null) { exhausted.Add(new JsonObject { ["mailbox"] = p.Mailbox, ["device_id"] = p.Device }); continue; }
                        if (target.Packages.Count > 0) target.Packages.RemoveAt(0);
                        grp.Claims[kp.Value.Ref] = (p.Mailbox, p.Device, d.Id);
                        claims.Add(new JsonObject { ["mailbox"] = p.Mailbox, ["device_id"] = p.Device, ["key_package"] = kp.Value.Package, ["key_package_ref"] = kp.Value.Ref });
                    }
                return new JsonObject { ["claims"] = claims, ["exhausted"] = exhausted };
            }
            case ("POST", "commits"): return Commit(grp, me, mine, DeviceOf(me, deviceId), body!);
            case ("POST", "epoch-ack"): DeviceOf(me, deviceId); return new JsonObject { ["ok"] = true };
            case ("POST", "resync"):
            {
                var d = DeviceOf(me, deviceId);
                grp.Leaves.Remove((me.Address, d.Id));
                return new JsonObject { ["ok"] = true };
            }
            case ("GET", "messages"):
            {
                DeviceOf(me, deviceId);
                if (mine.State != "active") throw new Fail(403, "not_member");
                var after = long.TryParse(query["after_seq"], out var a) ? a : 0;
                var visible = grp.Items.Where(i => (long)i["seq"]! > after && (long)i["seq"]! >= mine.FirstSeq
                    && !((long?)i["expires_at"] is { } exp && exp <= Now)).ToArray();
                var page = visible.Take(100).Select(i => i.DeepClone()).ToArray();
                return new JsonObject { ["items"] = new JsonArray(page), ["last_seq"] = grp.Seq, ["epoch"] = grp.Epoch, ["more"] = visible.Length > 100 };
            }
            case ("POST", "messages"): return Post(grp, me, mine, DeviceOf(me, deviceId), body!);
            case ("DELETE", "messages"):
            {
                var seq = long.Parse(parts[4]);
                var item = grp.Items.First(i => (long)i["seq"]! == seq);
                item["ciphertext"] = null;
                item["deleted_at"] = Now;
                return new JsonObject { ["deleted"] = true };
            }
            case ("POST", "attachments"):
            {
                DeviceOf(me, deviceId);
                var id = NewId("gat");
                grp.Attachments[id] = raw;
                return (201, new JsonObject { ["id"] = id, ["bytes"] = raw.Length, ["sha256"] = Hex(SHA256.HashData(raw)) });
            }
            case ("GET", "attachments"):
            {
                if (mine.State != "active") throw new Fail(403, "not_member");
                return grp.Attachments.TryGetValue(parts[4], out var blob) ? blob : throw new Fail(404, "not_found");
            }
        }
        throw new Fail(404, "not_found");
    }

    private string GroupApiIdSafe() => Convert.ToHexString(RandomNumberGenerator.GetBytes(13)).ToLowerInvariant()[..26];

    private IEnumerable<(string Kind, string Mailbox, string Device)> Pending(Grp g)
    {
        foreach (var m in g.Members.Values.Where(m => m.State is "active" or "keying"))
            foreach (var d in devices.Values.Where(d => d.Mailbox == m.Address && !d.Revoked && !g.Leaves.Contains((m.Address, d.Id))))
                yield return ("add", m.Address, d.Id);
        foreach (var (mailbox, device) in g.Leaves)
        {
            var live = g.Members.TryGetValue(mailbox, out var m) && m.State is "active" or "keying";
            var ok = devices.TryGetValue((mailbox, device), out var d) && !d.Revoked;
            if (!live || !ok) yield return ("remove", mailbox, device);
        }
    }

    private void Notice(Grp g, string kind, string? actor, JsonObject metadata) => g.Items.Add(new JsonObject
    {
        ["seq"] = ++g.Seq, ["type"] = "notice", ["kind"] = kind, ["actor"] = actor, ["metadata"] = metadata, ["created_at"] = Now,
    });

    private object Commit(Grp g, Box me, Member mine, Device d, JsonNode body)
    {
        var mayCommit = g.Activation == "provisioning" ? mine.Role == "owner" && mine.State == "keying" : mine.State == "active" && g.Leaves.Contains((me.Address, d.Id));
        if (!mayCommit) throw new Fail(403, "not_member");
        var commit = Convert.FromBase64String((string)body["commit"]!);
        var info = GC.InspectEnvelope(commit);
        if (info.Kind != "commit" || info.GroupId != g.Id || info.SenderMailbox != me.Address || info.SenderDevice != d.Id || (long)info.Epoch != (long)body["epoch"]!)
            throw new Fail(400, "binding_mismatch");
        GC.VerifyEnvelopeSignature(d.SignatureKey, commit, Convert.FromBase64String((string)body["envelope_signature"]!));
        if ((long)body["epoch"]! != g.Epoch || (long?)body["expected_revision"] != g.Revision)
            throw new Fail(409, "revision_conflict", new JsonObject { ["current_epoch"] = g.Epoch, ["revision"] = g.Revision });
        var welcome = (string?)body["welcome"];
        var adds = body["adds"]!.AsArray().Select(a => ((string)a!["mailbox"]!, (string)a["device_id"]!, ((string)a["key_package_ref"]!).ToLowerInvariant())).ToArray();
        var recipients = welcome is null ? [] : GC.WelcomeRecipients(Convert.FromBase64String(welcome)).Select(Hex).ToHashSet();
        if (!recipients.SetEquals(adds.Select(a => a.Item3))) throw new Fail(400, "binding_mismatch");
        var pending = Pending(g).ToArray();
        foreach (var (mailbox, device, reference) in adds)
            if (!pending.Contains(("add", mailbox, device)) || !g.Claims.TryGetValue(reference, out var claim) || claim.By != d.Id || claim.Mailbox != mailbox)
                throw new Fail(403, "not_permitted");
        var removes = body["removes"]!.AsArray().Select(r => ((string)r!["mailbox"]!, (string)r["device_id"]!)).ToArray();
        foreach (var r in removes)
            if (!pending.Contains(("remove", r.Item1, r.Item2)) && !(r.Item1 == me.Address && r.Item2 != d.Id && g.Leaves.Contains(r)))
                throw new Fail(403, "not_permitted");
        var seq = ++g.Seq;
        g.Items.Add(new JsonObject
        {
            ["seq"] = seq, ["type"] = "commit", ["epoch"] = g.Epoch, ["sender"] = me.Address, ["sender_device"] = d.Id,
            ["commit_id"] = (string)body["commit_id"]!, ["commit"] = (string)body["commit"]!,
            ["adds"] = new JsonArray(adds.Select(a => (JsonNode)new JsonObject { ["mailbox"] = a.Item1, ["device_id"] = a.Item2 }).ToArray()),
            ["removes"] = new JsonArray(removes.Select(r => (JsonNode)new JsonObject { ["mailbox"] = r.Item1, ["device_id"] = r.Item2 }).ToArray()),
            ["created_at"] = Now,
        });
        g.Epoch++;
        g.Revision++;
        foreach (var r in removes) g.Leaves.Remove(r);
        var activating = g.Activation == "provisioning";
        if (activating) g.Leaves.Add((me.Address, d.Id));
        foreach (var (mailbox, device, reference) in adds)
        {
            g.Leaves.Add((mailbox, device));
            g.Claims.Remove(reference);
            devices[(mailbox, device)].Welcomes.Add((NewId("wel"), g.Id, seq, g.Epoch, welcome!));
        }
        var activated = g.Members.Values.Where(m => m.State == "keying" && g.Leaves.Any(l => l.Mailbox == m.Address)).ToArray();
        foreach (var m in activated) { m.State = "active"; m.JoinedAt = Now; m.FirstSeq = seq + 1; m.Revision++; }
        if (activating)
        {
            g.Activation = "active";
            Notice(g, "group_created", me.Address, new JsonObject { ["name"] = g.Name });
        }
        foreach (var m in activated.Where(m => !(activating && m.Address == me.Address)))
            Notice(g, "member_joined", null, new JsonObject { ["address"] = m.Address });
        return (201, new JsonObject { ["seq"] = seq, ["epoch"] = g.Epoch, ["revision"] = g.Revision });
    }

    private object Post(Grp g, Box me, Member mine, Device d, JsonNode body)
    {
        if (mine.State != "active") throw new Fail(403, "not_member");
        if (mine.Role == "restricted" || g.PostPolicy == "admins" && mine.Role is not ("owner" or "admin")) throw new Fail(403, "posting_restricted");
        var cmid = (string)body["client_message_id"]!;
        if (g.Cmids.TryGetValue(me.Address + "|" + cmid, out var existing)) return (201, new JsonObject { ["seq"] = existing, ["id"] = "gm_" + existing });
        var ciphertext = Convert.FromBase64String((string)body["ciphertext"]!);
        GC.VerifyEnvelopeSignature(d.SignatureKey, ciphertext, Convert.FromBase64String((string)body["envelope_signature"]!));
        var info = GC.InspectEnvelope(ciphertext);
        if (info.Kind != "application" || info.GroupId != g.Id || info.SenderMailbox != me.Address || info.SenderDevice != d.Id || info.ClientMessageId != cmid)
            throw new Fail(400, "binding_mismatch");
        if ((long)body["epoch"]! != g.Epoch) throw new Fail(409, "epoch_conflict", new JsonObject { ["current_epoch"] = g.Epoch });
        if (Pending(g).Any(p => p.Kind == "remove")) throw new Fail(409, "key_activation_pending");
        if ((string?)body["enc_scheme"] != GC.EncScheme()) throw new Fail(400, "encryption_unsupported");
        var seq = ++g.Seq;
        long? expires = g.EphemeralSecs is { } secs ? Now + secs : null;
        g.Items.Add(new JsonObject
        {
            ["seq"] = seq, ["type"] = "application", ["id"] = "gm_" + seq, ["epoch"] = g.Epoch, ["sender"] = me.Address, ["sender_device"] = d.Id,
            ["client_message_id"] = cmid, ["ciphertext"] = (string)body["ciphertext"]!,
            ["attachments"] = new JsonArray((body["attachments"]?.AsArray() ?? []).Select(a => (JsonNode)new JsonObject { ["id"] = (string)a! }).ToArray()),
            ["reply_to"] = body["reply_to"]?.DeepClone(), ["edit_of"] = body["edit_of"]?.DeepClone(), ["created_at"] = Now, ["expires_at"] = expires,
        });
        g.Cmids[me.Address + "|" + cmid] = seq;
        return (201, new JsonObject { ["seq"] = seq, ["id"] = "gm_" + seq, ["expires_at"] = expires });
    }

    private JsonNode GroupJson(Grp g, Box me)
    {
        var m = g.Members[me.Address];
        var my = new JsonObject
        {
            ["role"] = m.Role, ["state"] = m.State, ["read_cursor"] = m.ReadCursor,
            ["unread"] = g.Items.Count(i => (string?)i["type"] == "application" && (long)i["seq"]! > m.ReadCursor && (long)i["seq"]! >= m.FirstSeq && (string?)i["sender"] != me.Address),
            ["notify"] = "all", ["invited_by"] = m.InvitedBy, ["joined_at"] = m.JoinedAt, ["revision"] = m.Revision,
        };
        if (m.State == "invited")
            return new JsonObject { ["id"] = g.Id, ["name"] = g.Name, ["description"] = g.Description, ["member_count"] = g.Members.Values.Count(x => x.State == "active"), ["join_policy"] = g.JoinPolicy, ["my"] = my };
        return new JsonObject
        {
            ["id"] = g.Id, ["name"] = g.Name, ["description"] = g.Description, ["join_policy"] = g.JoinPolicy, ["post_policy"] = g.PostPolicy,
            ["visibility"] = g.Visibility, ["members_can_invite"] = g.MembersCanInvite, ["members_can_change_timer"] = g.MembersCanChangeTimer,
            ["ephemeral_secs"] = g.EphemeralSecs, ["revision"] = g.Revision, ["epoch"] = g.Epoch, ["activation_state"] = g.Activation,
            ["member_count"] = g.Members.Values.Count(x => x.State is "active" or "keying"), ["max_members"] = 1000, ["created_by"] = g.CreatedBy,
            ["created_at"] = g.CreatedAt, ["last_at"] = g.Items.Count == 0 ? g.CreatedAt : (long)g.Items[^1]["created_at"]!, ["last_seq"] = g.Seq,
            ["deleted_at"] = g.DeletedAt, ["has_link"] = false, ["my"] = my,
        };
    }

    // attestation_payload(mailbox, device_id, signature_key) of crates/pigeonpost-groupcrypto.
    public static byte[] AttestationPayload(string mailbox, string deviceId, byte[] signatureKey)
    {
        using var stream = new MemoryStream();
        stream.Write("pigeonpost/groupcrypto/device-attestation/v1\0"u8);
        void Field(byte[] bytes) { stream.WriteByte((byte)(bytes.Length >> 8)); stream.WriteByte((byte)bytes.Length); stream.Write(bytes); }
        Field(Encoding.UTF8.GetBytes(mailbox));
        Field(Encoding.UTF8.GetBytes(deviceId));
        stream.WriteByte(0); stream.WriteByte(1);
        Field(signatureKey);
        return stream.ToArray();
    }
}
