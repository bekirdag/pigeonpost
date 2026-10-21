using System.Collections.ObjectModel;

namespace Pigeonpost.Core.Groups;

// One row of the group list: a group, or an invitation waiting for an answer.
public sealed record GroupRow(GroupInfo Info, string Preview, long Last)
{
    public string Id => Info.Id;
    public string Name => Info.DisplayName;
    public bool IsInvitation => Info.IsInvited;
    public bool IsKeying => Info.IsKeying;
    public int Unread => IsInvitation ? 0 : Info.My?.Unread ?? 0;
    public bool HasUnread => Unread > 0;
    public string UnreadLabel => Unread > 99 ? "99+" : Unread.ToString();
    public string Initials => ConversationPresentation.Initials(Name);
    public string ActivityLabel => ConversationPresentation.ActivityLabel(Last, DateTimeOffset.Now);
    public string Detail => IsInvitation ? $"Invitation · {Info.MemberCount ?? 0} member{(Info.MemberCount == 1 ? "" : "s")}"
        : IsKeying ? "Waiting for a member's device to add you…" : Preview;
    public bool Muted => Info.My?.Notify == "none" || Info.My?.MutedUntil is { } until && until > DateTimeOffset.UtcNow.ToUnixTimeSeconds();
    public string AccessibleSummary => $"{Name}. {Detail}" + (HasUnread ? $". {Unread} unread" : "");
}

// One line of the open group, ready to show.
public sealed record GroupLineRow(string Key, string Kind, string Text, string? Sender, string? SenderKey, bool Mine, long At,
    IReadOnlyList<LocalFile> Files, bool Disappears, bool Muted, bool Warning, long Seq)
{
    public bool IsMessage => Kind == "message";
    public bool IsSystem => !IsMessage;
    public bool HasSender => IsMessage && !Mine && Sender is not null;
    public string TimeLabel => At is > 0 and <= 253402300799 ? DateTimeOffset.FromUnixTimeSeconds(At).ToLocalTime().ToString("g") : "";
    public string Initials => ConversationPresentation.Initials(Sender ?? "");
    public bool HasFiles => Files.Count > 0;
}

public sealed record GroupMemberRow(string Address, string Name, string Role, string State, IReadOnlyList<DeviceSummary> Devices, bool IsMe, long Revision)
{
    public string RoleLabel => Role switch { "owner" => "Owner", "admin" => "Admin", "restricted" => "Can't post", _ => State == "invited" ? "Invited" : "" };
    public string DevicesLabel => Devices.Count == 0 ? "No device in the group yet"
        : string.Join("\n", Devices.Select(d => $"{d.DeviceId} · {GroupClient.Digits(d.Fingerprint)}"));
    public string Initials => ConversationPresentation.Initials(Name);
}

// The Groups page (SDS §4.2, docs/groups.md): list, invitations, the open group, its info, and the
// client loop that keeps keys current. Construct and use on the UI context.
public sealed class GroupsModel(PostboxClient postbox, Func<string, IGroupStateStore> stores) : ObservableObject, IDisposable
{
    // The group client finishes its work on the thread pool; list changes belong to the UI thread.
    private readonly SynchronizationContext? ui = SynchronizationContext.Current;
    private GroupClient? client;
    private Mailbox? mailbox;
    private IReadOnlyList<Contact> contacts = [];
    private IReadOnlyList<Mailbox> own = [];
    private readonly Dictionary<string, Dictionary<string, string>> handles = new(StringComparer.Ordinal); // group → address → handle
    private bool running;
    private string? error;
    private string? status;
    private GroupRow? selected;
    private string draft = "";
    private bool sending;
    private int generation;
    private CancellationTokenSource scope = new();

    public static bool LibraryAvailable { get; } = GroupClient.LibraryAvailable();
    public bool Enabled { get; private set; }
    public bool Persistent => client?.Persistent ?? false;
    public ObservableCollection<GroupRow> Groups { get; } = [];
    public ObservableCollection<GroupLineRow> Lines { get; } = [];
    public ObservableCollection<GroupClient.OutgoingFile> DraftFiles { get; } = [];
    public string? DeviceFingerprint => client?.Fingerprint;
    public string? DeviceId => client?.DeviceId;
    public string Mailbox => mailbox?.Address ?? "";
    public int Invitations => Groups.Count(g => g.IsInvitation);
    public int UnreadTotal => Groups.Sum(g => g.Unread);
    public string? Error { get => error; private set { if (Set(ref error, value)) Changed(nameof(HasError)); } }
    public bool HasError => Error is not null;
    public string? Status { get => status; set => Set(ref status, value); }
    public bool IsSending { get => sending; private set { if (Set(ref sending, value)) Changed(nameof(CanSend)); } }
    public string Draft { get => draft; set { if (Set(ref draft, value)) Changed(nameof(CanSend)); } }
    public GroupRow? Selected => selected;
    public GroupInfo? Info => selected?.Info;
    public string Title => Info?.DisplayName ?? "Groups";
    public string Subtitle => Info is null ? "" : Info.IsInvited ? "Invitation" : $"{Info.MemberCount ?? 0} member{(Info.MemberCount == 1 ? "" : "s")}"
        + (Info.EphemeralSecs is > 0 ? " · " + Timers.Banner(Info.EphemeralSecs) : "");
    public bool HasSelection => selected is not null;
    public bool ShowsInvitation => selected?.IsInvitation == true;
    public bool ShowsComposer => Info?.CanPost == true;
    public bool ShowsAnnouncementBar => Info is { IsActive: true } && !Info.CanPost;
    public string AnnouncementText => Info?.Role == "restricted" ? "An admin stopped you from posting in this group." : "Only admins can send messages";
    public bool ShowsKeying => Info is { IsKeying: true } || Info is { IsActive: true } && client?.InGroup(Info.Id) == false;
    public bool CanSend => !IsSending && Info?.CanPost == true && client?.InGroup(Info.Id) == true && (Draft.Trim().Length > 0 || DraftFiles.Count > 0);
    public bool CanCall => Info is { IsActive: true };
    public string TimerBanner => Timers.Banner(Info?.EphemeralSecs);
    public string EmptyText => Groups.Count == 0 ? "No groups yet. Use New group to start one." : "";
    public event EventHandler? LinesChanged;

    // Called when the mailbox, the account's features or the contact list change.
    public async Task ActivateAsync(Mailbox? acting, ServerFeatures features, IReadOnlyList<Mailbox> mailboxes, IReadOnlyList<Contact> known)
    {
        contacts = known;
        own = mailboxes;
        var enabled = acting is not null && features.Has(ServerFeatures.Groups) && LibraryAvailable;
        if (enabled == Enabled && acting?.Address == mailbox?.Address) return;
        scope.Cancel();
        scope.Dispose();
        scope = new();
        generation++;
        client?.Dispose();
        client = null;
        mailbox = acting;
        Enabled = enabled;
        selected = null;
        Groups.Clear();
        Lines.Clear();
        handles.Clear();
        Error = null;
        NotifyAll();
        if (!enabled || acting is null) return;
        var next = new GroupClient(new GroupApi(postbox, acting.Address), stores(acting.Address));
        next.HistoryChanged += gid =>
        {
            if (ui is null || SynchronizationContext.Current == ui) { if (gid == selected?.Id) RebuildLines(); }
            else ui.Post(_ => { if (ReferenceEquals(client, next) && gid == selected?.Id) RebuildLines(); }, null);
        };
        client = next;
        var version = generation;
        try
        {
            await next.InitAsync(scope.Token);
            if (version != generation) return;
            await RefreshAsync();
        }
        catch (OperationCanceledException) { }
        catch (Exception ex) { if (version == generation) Error = "Groups aren't available on this device right now. " + Describe(ex); }
        NotifyAll();
    }

    public void SetContacts(IReadOnlyList<Contact> known) => contacts = known;

    // The client loop (docs/groups.md §3): key packages, welcomes, each group's messages in seq
    // order, and pending adds and removals. Runs on a timer and after every action.
    public async Task RefreshAsync()
    {
        if (client is not { } c || running) return;
        running = true;
        var version = generation;
        var token = scope.Token;
        try
        {
            await c.TopUpKeyPackagesAsync(token);
            await c.ProcessWelcomesAsync(token);
            var list = await c.Api.ListAsync(token);
            if (version != generation) return;
            foreach (var g in list.Where(g => g.IsActive || g.ActivationState == "provisioning" && g.IsOwner))
            {
                try
                {
                    if (g.ActivationState == "provisioning" && g.IsOwner) { await c.ActivateAsync(g.Id, token); continue; }
                    if (!c.InGroup(g.Id)) continue; // waiting for a welcome
                    await c.SyncAsync(g.Id, token);
                    await c.ProcessPendingAsync(g.Id, token);
                }
                catch (PostboxException ex) when (ex.Code is "not_member" or "group_deleted" or "not_found") { }
            }
            await c.PruneExpiredAsync();
            if (version != generation) return;
            RebuildRows(list);
            Error = null;
        }
        catch (OperationCanceledException) { }
        catch (PostboxException ex) when (ex.Code is "unknown_device" or "device_revoked")
        {
            // This device was revoked elsewhere: start again as a new device; members are shown the change.
            try { await c.ReEnrolAsync(token); Status = "This device was registered again for groups. Earlier group messages stay on the old device."; }
            catch (Exception inner) when (inner is not OperationCanceledException) { Error = Describe(inner); }
        }
        catch (Exception ex) { if (version == generation) Error = Describe(ex); }
        finally { running = false; }
    }

    private void RebuildRows(IReadOnlyList<GroupInfo> list)
    {
        var rows = list.Where(g => !g.IsDeleted).Select(g =>
        {
            var lines = client?.HistoryOf(g.Id) ?? [];
            var last = lines.LastOrDefault(l => l.Kind == "message" && !l.Deleted && l.ReplacedBy is null);
            var preview = last is null ? g.IsActive ? "No messages yet" : "" : (last.Mine ? "You: " : ShortName(last.Sender) + ": ")
                + (string.IsNullOrWhiteSpace(last.Text) && last.Files is { Count: > 0 } f ? f[0].Name : (last.Text ?? "").ReplaceLineEndings(" "));
            if (last is not null && IsBlocked(last.Sender)) preview = "";
            return new GroupRow(g, preview, Math.Max(g.LastAt ?? 0, last?.CreatedAt ?? 0));
        }).OrderByDescending(r => r.IsInvitation).ThenByDescending(r => r.Last).ToArray();
        StableList.Reconcile(Groups, rows, r => r.Id, (a, b) => a == b);
        if (selected is { } s)
        {
            selected = Groups.FirstOrDefault(r => r.Id == s.Id);
            RebuildLines();
        }
        NotifyAll();
    }

    public async Task SelectAsync(GroupRow? row)
    {
        selected = row;
        Draft = "";
        DraftFiles.Clear();
        RebuildLines();
        NotifyAll();
        if (row is null || client is not { } c || !row.Info.IsActive) return;
        await LoadHandlesAsync(row.Id);
        RebuildLines();
        await MarkReadAsync();
    }

    public async Task MarkReadAsync()
    {
        if (client is not { } c || selected is not { Info.IsActive: true } row) return;
        var seq = c.HistoryOf(row.Id).Where(l => l.Kind == "message").Select(l => l.Seq).DefaultIfEmpty(0).Max();
        if (seq <= (row.Info.My?.ReadCursor ?? 0) && row.Unread == 0) return;
        try { await c.Api.MarkReadAsync(row.Id, Math.Max(seq, row.Info.LastSeq ?? seq), scope.Token); }
        catch (Exception ex) when (ex is not OperationCanceledException) { }
    }

    private async Task LoadHandlesAsync(string gid)
    {
        if (client is not { } c) return;
        try
        {
            var members = await c.Api.MembersAsync(gid, scope.Token);
            handles[gid] = members.Where(m => m.Handle is not null).ToDictionary(m => m.Address, m => m.Handle!, StringComparer.Ordinal);
        }
        catch (Exception ex) when (ex is not OperationCanceledException) { }
    }

    private void RebuildLines()
    {
        if (client is not { } c || selected is not { } row)
        {
            Lines.Clear();
            LinesChanged?.Invoke(this, EventArgs.Empty);
            return;
        }
        var now = DateTimeOffset.UtcNow.ToUnixTimeSeconds();
        var rows = new List<GroupLineRow>();
        foreach (var line in c.HistoryOf(row.Id))
        {
            if (line.ExpiresAt is { } at && at <= now) continue;
            if (Render(line) is { } r) rows.Add(r);
        }
        StableList.Reconcile(Lines, rows, r => r.Key, (a, b) => a == b && a.Files.SequenceEqual(b.Files));
        LinesChanged?.Invoke(this, EventArgs.Empty);
    }

    // ---- names and words ---------------------------------------------------------------------

    public string NameOf(string? address)
    {
        if (string.IsNullOrEmpty(address)) return "Someone";
        if (address == mailbox?.Address) return "You";
        if (own.FirstOrDefault(m => m.Address == address) is { } mine) return mine.DisplayName;
        var handle = selected is { } s && handles.TryGetValue(s.Id, out var map) ? map.GetValueOrDefault(address) : null;
        var key = handle ?? address;
        var alias = ConversationBuilder.FindContact(key, contacts)?.Alias ?? ConversationBuilder.FindContact(address, contacts)?.Alias;
        if (!string.IsNullOrWhiteSpace(alias)) return alias;
        return handle is not null ? PostAddress.DisplayName(handle) : address.Length > 12 ? address[..12] + "…" : address;
    }

    public string KeyOf(string address) => selected is { } s && handles.TryGetValue(s.Id, out var map) && map.GetValueOrDefault(address) is { } h ? h : address;

    private string ShortName(string? address) => NameOf(address).Split(' ')[0];
    private bool IsBlocked(string? address) => address is not null && address != mailbox?.Address
        && (BlockRules.IsBlocked(address, contacts) || BlockRules.IsBlocked(KeyOf(address), contacts));

    private GroupLineRow? Render(GroupLine l)
    {
        string Who(string? a) => NameOf(a);
        var key = $"{l.Kind}:{l.Seq}:{l.Order}:{l.Cmid}";
        switch (l.Kind)
        {
            case "message":
                if (l.ReplacedBy is not null) return null; // an edit replaced it
                var blocked = IsBlocked(l.Sender);
                var text = l.Deleted ? "Message deleted"
                    : l.Undecryptable ? "This message could not be decrypted on this device."
                    : blocked ? "Hidden: you blocked this sender."
                    : (l.EditOf is not null ? "(edited) " : "") + (l.Text ?? "");
                return new GroupLineRow(key, "message", text, l.Mine ? "You" : Who(l.Sender), l.Sender is null ? null : KeyOf(l.Sender), l.Mine || l.Sender == mailbox?.Address,
                    l.CreatedAt, l.Deleted || blocked || l.Undecryptable ? [] : l.Files ?? [], l.ExpiresAt is not null, l.Deleted || l.Undecryptable || blocked, false, l.Seq);
            case "notice":
                return NoticeText(l) is { } notice ? new GroupLineRow(key, "notice", notice, null, null, false, l.CreatedAt, [], l.ExpiresAt is not null, true, false, l.Seq) : null;
            case "commit":
                var changes = new List<string>();
                foreach (var d in l.NewDevices ?? [])
                    changes.Add((d.Mailbox == mailbox?.Address ? d.DeviceId == client?.DeviceId ? "This device was added" : "You added a new device" : Who(d.Mailbox) + " added a new device")
                        + " · " + GroupClient.Digits(d.Fingerprint));
                foreach (var d in l.Removed ?? [])
                    if (d.Mailbox == mailbox?.Address && d.DeviceId != client?.DeviceId) changes.Add("One of your devices was removed from the group");
                return changes.Count == 0 ? null
                    : new GroupLineRow(key, "device", string.Join("\n", changes) + $"\nKey change by {Who(l.Committer?.Mailbox)}", null, null, false, l.CreatedAt, [], false, true, false, l.Seq);
            case "warning":
                var warning = l.Warning switch
                {
                    "commit_rejected" => "A key change could not be verified, so this device stopped reading this group to stay safe.",
                    "membership_mismatch" => "The postbox's account of who joined or left differs from the verified key change. Check Group info.",
                    "sender_mismatch" => "A message whose sender could not be verified was hidden.",
                    "resynced" => "This device lost its keys for this group and is rejoining. Earlier messages may be missing.",
                    "safety_number_changed" => "The safety number changed because a member's devices changed. Compare it again in Group info.",
                    _ => null,
                };
                return warning is null ? null : new GroupLineRow(key, "warning", warning, null, null, false, l.CreatedAt, [], false, true, true, l.Seq);
        }
        return null;
    }

    private string? NoticeText(GroupLine l)
    {
        var m = l.Metadata ?? [];
        var actor = NameOf(l.Actor);
        var target = NameOf(m.GetValueOrDefault("address"));
        return l.Notice switch
        {
            "you_joined" => "You joined the group",
            "group_created" => $"{actor} created the group",
            "member_invited" => $"{actor} invited {target}",
            "member_joined" => $"{target} joined",
            "member_left" => $"{target} left",
            "member_removed" => $"{actor} removed {target}",
            "member_banned" => $"{actor} banned {target}",
            "role_changed" => m.GetValueOrDefault("role") switch
            {
                "admin" => $"{actor} made {target} an admin",
                "restricted" => $"{actor} stopped {target} from posting",
                _ => $"{actor} made {target} a member",
            },
            "settings_changed" => m.GetValueOrDefault("post_policy") == "admins" && (m.GetValueOrDefault("changed") ?? "").Contains("post_policy")
                ? "Posting set to admins only"
                : (m.GetValueOrDefault("changed") ?? "").Contains("post_policy") ? "Everyone can post again" : $"{actor} changed the group settings",
            "ownership_transferred" => $"{actor} made {target} the owner",
            "device_added" => $"{target} added a new device",
            "device_removed" => $"{target} removed a device",
            "timer_changed" => $"{actor} set disappearing messages to {Timers.Label(int.TryParse(m.GetValueOrDefault("ephemeral_secs"), out var secs) ? secs : null)}",
            "call_started" => $"{actor} started a call",
            "call_ended" => "Call ended",
            "link_reset" => $"{actor} reset the invite link",
            _ => null,
        };
    }

    // ---- actions -----------------------------------------------------------------------------

    public async Task<bool> SendAsync()
    {
        if (!CanSend || client is not { } c || selected is not { } row) return false;
        var text = Draft.Trim();
        var files = DraftFiles.ToArray();
        IsSending = true;
        Error = null;
        try
        {
            await c.SendAsync(row.Id, text, files, scope.Token);
            if (selected?.Id == row.Id) { Draft = ""; DraftFiles.Clear(); }
            RebuildLines();
            return true;
        }
        catch (OperationCanceledException) { return false; }
        catch (Exception ex) { Error = Describe(ex); return false; }
        finally { IsSending = false; }
    }

    public void AddFile(GroupClient.OutgoingFile file)
    {
        if (file.Bytes.Length is 0 or > PostboxClient.MaxAttachmentBytes) { Error = "Choose a file between 1 byte and 25 MB."; return; }
        DraftFiles.Add(file);
        Changed(nameof(CanSend));
    }

    public void RemoveFile(GroupClient.OutgoingFile file) { DraftFiles.Remove(file); Changed(nameof(CanSend)); }

    public async Task<byte[]?> OpenFileAsync(LocalFile file)
    {
        if (client is not { } c || selected is not { } row) return null;
        try { return await c.OpenFileAsync(row.Id, file, scope.Token); }
        catch (Exception ex) when (ex is not OperationCanceledException) { Error = "Could not open this file. " + Describe(ex); return null; }
    }

    public async Task<GroupInfo?> CreateAsync(GroupSettings settings, IReadOnlyList<string> invite)
    {
        if (client is not { } c) return null;
        Error = null;
        try
        {
            var created = await c.CreateGroupAsync(settings, invite.Select(PostAddress.Canonical).ToArray(), scope.Token);
            var unresolved = created.Invitations?.Where(i => i.Status is not ("invited" or "already_member")).Select(i => i.Address).ToArray() ?? [];
            if (unresolved.Length > 0) Status = "Not invited: " + string.Join(", ", unresolved);
            await RefreshAsync();
            await SelectAsync(Groups.FirstOrDefault(g => g.Id == created.Group.Id));
            return created.Group;
        }
        catch (Exception ex) when (ex is not OperationCanceledException) { Error = Describe(ex); return null; }
    }

    public Task AnswerInvitationAsync(bool accept) => Act(async (c, row) =>
    {
        await c.Api.AnswerInvitationAsync(row.Id, accept, scope.Token);
        Status = accept ? "Joined. A member's device adds you in a moment." : "Invitation declined.";
        if (!accept) selected = null;
    });

    public Task LeaveAsync() => Act(async (c, row) =>
    {
        await c.Api.LeaveAsync(row.Id, scope.Token);
        await c.ForgetAsync(row.Id);
        selected = null;
        Status = "You left the group.";
    });

    public Task DeleteAsync() => Act(async (c, row) =>
    {
        await c.Api.DeleteAsync(row.Id, scope.Token);
        await c.ForgetAsync(row.Id);
        selected = null;
        Status = "Group deleted.";
    });

    public Task SetTimerAsync(int? seconds) => Update(new Dictionary<string, object?> { ["ephemeral_secs"] = seconds });
    public Task SetPostPolicyAsync(bool adminsOnly) => Update(new Dictionary<string, object?> { ["post_policy"] = adminsOnly ? "admins" : "everyone" });
    public Task SetMembersCanInviteAsync(bool allowed) => Update(new Dictionary<string, object?> { ["members_can_invite"] = allowed });
    public Task RenameAsync(string name, string? description) => Update(new Dictionary<string, object?> { ["name"] = name.Trim(), ["description"] = string.IsNullOrWhiteSpace(description) ? null : description.Trim() });

    private Task Update(Dictionary<string, object?> changes) => Act(async (c, row) =>
    {
        try { await c.Api.UpdateAsync(row.Id, row.Info.Revision ?? 0, changes, scope.Token); }
        catch (PostboxException ex) when (ex.Code == "revision_conflict")
        {
            var fresh = await c.Api.GetAsync(row.Id, scope.Token);
            await c.Api.UpdateAsync(row.Id, fresh.Revision ?? 0, changes, scope.Token);
        }
    });

    public async Task<IReadOnlyList<InviteResult>> InviteAsync(IReadOnlyList<string> addresses)
    {
        IReadOnlyList<InviteResult> results = [];
        await Act(async (c, row) => { results = await c.Api.InviteAsync(row.Id, addresses.Select(PostAddress.Canonical).ToArray(), scope.Token); });
        return results;
    }

    public Task SetMemberAsync(GroupMemberRow member, string? role, string? state) => Act(async (c, row) =>
    {
        await c.Api.UpdateMemberAsync(row.Id, member.Address, member.Revision, role, state, scope.Token);
        // A removal is committed by any member device; this one does it straight away.
        if (state is "removed" or "banned") await c.ProcessPendingAsync(row.Id, scope.Token, noJitter: true);
    });

    private async Task Act(Func<GroupClient, GroupRow, Task> work)
    {
        if (client is not { } c || selected is not { } row) return;
        Error = null;
        try
        {
            await work(c, row);
            await RefreshAsync();
        }
        catch (OperationCanceledException) { }
        catch (Exception ex) { Error = Describe(ex); }
        NotifyAll();
    }

    // Group info: members with roles and their devices' fingerprints, and the group safety number.
    public async Task<(IReadOnlyList<GroupMemberRow> Members, string? SafetyNumber)> LoadInfoAsync()
    {
        if (client is not { } c || selected is not { } row) return ([], null);
        try
        {
            var members = await c.Api.MembersAsync(row.Id, scope.Token);
            handles[row.Id] = members.Where(m => m.Handle is not null).ToDictionary(m => m.Address, m => m.Handle!, StringComparer.Ordinal);
            var leaves = c.Members(row.Id);
            var rows = members.Select(m => new GroupMemberRow(m.Address, NameOf(m.Address), m.Role ?? "member", m.State ?? "active",
                    leaves.Where(l => l.Mailbox == m.Address).ToArray(), m.Address == mailbox?.Address, m.Revision ?? 0))
                .OrderBy(m => m.Role switch { "owner" => 0, "admin" => 1, _ => 2 }).ThenBy(m => m.Name, StringComparer.OrdinalIgnoreCase).ToArray();
            return (rows, c.SafetyNumberOf(row.Id));
        }
        catch (Exception ex) when (ex is not OperationCanceledException) { Error = Describe(ex); return ([], null); }
    }

    // "Call in progress — Join" in the group header (calls service, same origin as the inbox).
    public async Task<GroupCallStatus?> CallStatusAsync(Uri callsOrigin)
    {
        if (selected is not { Info.IsActive: true } row || mailbox is null) return null;
        try
        {
            return await postbox.GetCallsJsonAsync<GroupCallStatus>(new Uri(callsOrigin,
                "/calls/v1/groups/" + Uri.EscapeDataString(row.Id) + "/call?from=" + Uri.EscapeDataString(mailbox.Address)), scope.Token);
        }
        catch (Exception ex) when (ex is not OperationCanceledException) { return null; }
    }

    private void NotifyAll()
    {
        foreach (var name in new[] { nameof(Selected), nameof(Info), nameof(Title), nameof(Subtitle), nameof(HasSelection), nameof(ShowsInvitation),
            nameof(ShowsComposer), nameof(ShowsAnnouncementBar), nameof(AnnouncementText), nameof(ShowsKeying), nameof(CanSend), nameof(CanCall), nameof(TimerBanner),
            nameof(Invitations), nameof(UnreadTotal), nameof(EmptyText), nameof(Enabled), nameof(DeviceFingerprint), nameof(Persistent) })
            Changed(name);
    }

    private static string Describe(Exception ex) => ex switch
    {
        PostboxException p => p.Message,
        Pigeonpost.GroupCrypto.GroupCryptoException.Failure f => "The group keys could not be updated (" + f.code + ").",
        HttpRequestException or TaskCanceledException => "Could not connect. Check your internet connection and try again.",
        _ => "Could not complete that action. Please try again.",
    };

    public void Dispose()
    {
        scope.Cancel();
        scope.Dispose();
        client?.Dispose();
    }
}
