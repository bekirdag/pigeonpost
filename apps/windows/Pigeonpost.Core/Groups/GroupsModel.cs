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
    private GroupClient? refreshing;
    private AuthorizationLease? authorization;
    private bool disposed;
    private string? error;
    private string? status;
    private GroupRow? selected;
    private string draft = "";
    private bool sending;
    private int generation;
    private int selectionGeneration;
    private CancellationTokenSource scope = new();
    private GroupClient? ActiveClient => client is { IsActive: true } c ? c : null;
    private bool IsCurrent(GroupClient c, int version) => !disposed && version == generation && ReferenceEquals(client, c) && c.IsActive;
    private bool IsSelectionCurrent(GroupClient c, int version, int intent, string gid) =>
        IsCurrent(c, version) && intent == selectionGeneration && selected?.Id == gid;
    private T CurrentValue<T>(Func<GroupClient, T> read, T fallback)
    {
        if (ActiveClient is not { } c) return fallback;
        try { return read(c); } catch (AuthorizationRetiredException) { return fallback; }
    }

    public static bool LibraryAvailable { get; } = GroupClient.LibraryAvailable();
    public bool Enabled { get; private set; }
    // True once this inbox's encrypted group client finished starting.
    public bool IsReadyFor(string mailbox) => !disposed && Enabled && ActiveClient is not null && this.mailbox?.Address == mailbox && refreshing is null;
    public bool Persistent => CurrentValue(c => c.Persistent, false);
    public ObservableCollection<GroupRow> Groups { get; } = [];
    public ObservableCollection<GroupLineRow> Lines { get; } = [];
    public ObservableCollection<GroupClient.OutgoingFile> DraftFiles { get; } = [];
    public string? DeviceFingerprint => CurrentValue(c => c.Fingerprint, null);
    public string? DeviceId => CurrentValue(c => c.DeviceId, null);
    public string Mailbox => mailbox?.Address ?? "";
    public int Invitations => Groups.Count(g => g.IsInvitation);
    public int UnreadTotal => Groups.Sum(g => g.Unread);
    public string? Error { get => error; private set { if (Set(ref error, value)) Changed(nameof(HasError)); } }
    public bool HasError => Error is not null;
    public string? Status { get => status; set => Set(ref status, value); }
    public bool IsSending { get => sending; private set { if (Set(ref sending, value)) Changed(nameof(CanSend)); } }
    public string Draft { get => draft; set { if (Set(ref draft, value)) Changed(nameof(CanSend)); } }
    public GroupRow? Selected => ActiveClient is null ? null : selected;
    public GroupInfo? Info => Selected?.Info;
    public string Title => Info?.DisplayName ?? "Groups";
    public string Subtitle => Info is null ? "" : Info.IsInvited ? "Invitation" : $"{Info.MemberCount ?? 0} member{(Info.MemberCount == 1 ? "" : "s")}"
        + (Info.EphemeralSecs is > 0 ? " · " + Timers.Banner(Info.EphemeralSecs) : "");
    public bool HasSelection => Selected is not null;
    public bool ShowsInvitation => Selected?.IsInvitation == true;
    public bool ShowsComposer => Info?.CanPost == true;
    public bool ShowsAnnouncementBar => Info is { IsActive: true } && !Info.CanPost;
    public string AnnouncementText => Info?.Role == "restricted" ? "An admin stopped you from posting in this group." : "Only admins can send messages";
    public bool ShowsKeying => Info is { IsKeying: true } || Info is { IsActive: true } info && !CurrentValue(c => c.InGroup(info.Id), false);
    public bool CanSend => !IsSending && Info is { CanPost: true } info && CurrentValue(c => c.InGroup(info.Id), false) && (Draft.Trim().Length > 0 || DraftFiles.Count > 0);
    public bool CanCall => Info is { IsActive: true };
    public string TimerBanner => Timers.Banner(Info?.EphemeralSecs);
    public string EmptyText => Groups.Count == 0 ? "No groups yet. Use New group to start one." : "";
    public event EventHandler? LinesChanged;

    // Called when the mailbox, the account's features or the contact list change.
    public async Task ActivateAsync(Mailbox? acting, ServerFeatures features, IReadOnlyList<Mailbox> mailboxes, IReadOnlyList<Contact> known)
    {
        if (disposed) return;
        contacts = known;
        own = mailboxes;
        var enabled = acting is not null && features.Has(ServerFeatures.Groups) && LibraryAvailable;
        var accepted = postbox.AcceptedAuthorization;
        if (enabled == Enabled && acting?.Address == mailbox?.Address && ReferenceEquals(accepted, authorization)
            && (!enabled || ActiveClient is not null)) return;
        generation++;
        selectionGeneration++;
        var prior = client;
        client = null;
        prior?.Dispose();
        scope.Cancel();
        scope.Dispose();
        scope = new();
        authorization = accepted;
        refreshing = null;
        if (carriedDraft is { } carry && acting is not null && acting.Address != carry.Mailbox) carriedDraft = null;
        mailbox = acting;
        Enabled = enabled;
        selected = null;
        Draft = "";
        DraftFiles.Clear();
        IsSending = false;
        Status = null;
        Groups.Clear();
        Lines.Clear();
        handles.Clear();
        Error = null;
        NotifyAll();
        if (!enabled || acting is null) return;
        var next = new GroupClient(new GroupApi(postbox, acting.Address), stores(acting.Address));
        var version = generation;
        next.HistoryChanged += gid =>
        {
            void Apply() { if (IsCurrent(next, version) && gid == selected?.Id) RebuildLines(); }
            if (ui is null || SynchronizationContext.Current == ui) Apply();
            else ui.Post(_ => Apply(), null);
        };
        client = next;
        try
        {
            await next.InitAsync(scope.Token);
            if (!IsCurrent(next, version)) return;
            await RefreshAsync();
        }
        catch (OperationCanceledException) { }
        catch (Exception ex) { if (IsCurrent(next, version)) Error = "Groups aren't available on this device right now. " + Describe(ex); }
        if (IsCurrent(next, version)) NotifyAll();
    }

    // An unsent group draft survives one deliberate same-person grant replacement. It returns only
    // when that same group is opened again under a different live grant for the same inbox, after
    // the fresh membership read above; sending stays a separate explicit action.
    private sealed record CarriedDraft(string Mailbox, string Group, string Text, GroupClient.OutgoingFile[] Files, AuthorizationLease Prior);
    private CarriedDraft? carriedDraft;
    public bool ReadyForGrantReplacement => !IsSending;
    public bool HasCarriedDraft => carriedDraft is not null;
    public bool CarryUnsentWork(AuthorizationLease prior)
    {
        if (disposed || !ReferenceEquals(authorization, prior)) return false;
        carriedDraft = mailbox is { } box && selected is { } row && (Draft.Length > 0 || DraftFiles.Count > 0)
            ? new(box.Address, row.Id, Draft, DraftFiles.ToArray(), prior) : null;
        return true;
    }
    public void DropCarriedWork() => carriedDraft = null;
    private void RestoreCarried(string gid)
    {
        if (carriedDraft is not { } carry || mailbox?.Address != carry.Mailbox || authorization is not { IsActive: true } current
            || ReferenceEquals(current, carry.Prior) || current.SharesAuthorization(carry.Prior)) return;
        if (carry.Group != gid) return;
        carriedDraft = null;
        Draft = carry.Text;
        foreach (var file in carry.Files) DraftFiles.Add(file);
        Changed(nameof(CanSend));
    }

    // Opens an organization room only after a fresh read confirms its exact organization and live
    // membership. An owned provisioning room is activated with real MLS first. A refusal propagates.
    public async Task<bool> PrepareOrganizationRoomAsync(string groupId, string organizationId)
    {
        if (disposed || ActiveClient is not { } c) return false;
        var version = generation; var token = scope.Token;
        var fresh = await c.Api.GetAsync(groupId, token);
        if (!IsCurrent(c, version)) return false;
        if (fresh.Id != groupId || fresh.OrgId != organizationId || fresh.DeletedAt is not null || fresh.State is not ("active" or "keying")
            || fresh.Role is not ("owner" or "admin" or "member" or "restricted")) throw new PostboxException(403, "not_member");
        if (fresh.ActivationState == "provisioning" && fresh.IsOwner)
        {
            await c.ActivateAsync(groupId, token);
            if (!IsCurrent(c, version)) return false;
        }
        await RefreshAsync();
        if (!IsCurrent(c, version) || Groups.FirstOrDefault(g => g.Id == groupId) is not { } row) return false;
        await SelectAsync(row);
        return IsCurrent(c, version) && selected?.Id == groupId;
    }

    public void SetContacts(IReadOnlyList<Contact> known) => contacts = known;

    // The client loop (docs/groups.md §3): key packages, welcomes, each group's messages in seq
    // order, and pending adds and removals. Runs on a timer and after every action.
    public async Task RefreshAsync()
    {
        if (ActiveClient is not { } c || ReferenceEquals(refreshing, c)) return;
        refreshing = c;
        var version = generation;
        var token = scope.Token;
        try
        {
            await c.TopUpKeyPackagesAsync(token);
            await c.ProcessWelcomesAsync(token);
            var list = await c.Api.ListAsync(token);
            if (!IsCurrent(c, version)) return;
            var refused = new HashSet<string>(StringComparer.Ordinal);
            foreach (var g in list.Where(g => g.IsActive || g.ActivationState == "provisioning" && g.IsOwner))
            {
                try
                {
                    if (g.ActivationState == "provisioning" && g.IsOwner) { await c.ActivateAsync(g.Id, token); continue; }
                    if (!c.InGroup(g.Id)) continue; // waiting for a welcome
                    await c.SyncAsync(g.Id, token);
                    await c.ProcessPendingAsync(g.Id, token);
                }
                catch (PostboxException ex) when (ex.StatusCode is 403 or 404 || ex.Code is "not_member" or "group_deleted" or "not_found") { refused.Add(g.Id); }
            }
            await c.PruneExpiredAsync();
            if (!IsCurrent(c, version)) return;
            RebuildRows(list.Where(g => !refused.Contains(g.Id)).ToArray());
            Error = null;
        }
        catch (OperationCanceledException) { }
        catch (PostboxException ex) when (ex.Code is "unknown_device" or "device_revoked")
        {
            // This device was revoked elsewhere: start again as a new device; members are shown the change.
            try { await c.ReEnrolAsync(token); if (IsCurrent(c, version)) Status = "This device was registered again for groups. Earlier group messages stay on the old device."; }
            catch (Exception inner) when (inner is not OperationCanceledException) { if (IsCurrent(c, version)) Error = Describe(inner); }
        }
        catch (Exception ex) { if (IsCurrent(c, version)) Error = Describe(ex); }
        finally { if (ReferenceEquals(refreshing, c)) refreshing = null; }
    }

    private void RebuildRows(IReadOnlyList<GroupInfo> list)
    {
        var rows = list.Where(g => !g.IsDeleted).Select(g =>
        {
            var lines = g.IsActive ? ActiveClient?.HistoryOf(g.Id) ?? [] : [];
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
        if (disposed) return;
        var intent = ++selectionGeneration;
        var version = generation;
        var c = ActiveClient;
        var token = scope.Token;
        selected = null;
        Draft = "";
        DraftFiles.Clear();
        Error = null;
        RebuildLines();
        NotifyAll();
        if (row is null || c is null) return;
        if (!row.Info.IsActive)
        {
            selected = row;
            NotifyAll();
            return;
        }
        try
        {
            var fresh = await c.Api.GetAsync(row.Id, token);
            if (!IsCurrent(c, version) || intent != selectionGeneration) return;
            if (fresh.Id != row.Id || !fresh.IsActive) { RefuseSelection(row.Id); return; }
            selected = row with { Info = fresh };
            await LoadHandlesAsync(c, row.Id, version, intent, token);
            if (!IsSelectionCurrent(c, version, intent, row.Id)) return;
            RestoreCarried(row.Id);
            RebuildLines();
            NotifyAll();
            await MarkReadAsync();
        }
        catch (OperationCanceledException) { }
        catch (PostboxException ex) when (ex.StatusCode is 401 or 403 or 404 or 410)
        {
            if (IsCurrent(c, version) && intent == selectionGeneration) RefuseSelection(row.Id);
        }
        catch (Exception ex) { if (IsCurrent(c, version) && intent == selectionGeneration) Error = Describe(ex); }
    }

    private void RefuseSelection(string gid)
    {
        selected = null;
        var old = Groups.FirstOrDefault(g => g.Id == gid);
        if (old is not null) Groups.Remove(old);
        Lines.Clear();
        Error = "You no longer have access to this group. Refresh the list to try again.";
        NotifyAll();
        LinesChanged?.Invoke(this, EventArgs.Empty);
    }

    public async Task MarkReadAsync()
    {
        if (ActiveClient is not { } c || selected is not { Info.IsActive: true } row) return;
        var seq = c.HistoryOf(row.Id).Where(l => l.Kind == "message").Select(l => l.Seq).DefaultIfEmpty(0).Max();
        if (seq <= (row.Info.My?.ReadCursor ?? 0) && row.Unread == 0) return;
        try { await c.Api.MarkReadAsync(row.Id, Math.Max(seq, row.Info.LastSeq ?? seq), scope.Token); }
        catch (Exception ex) when (ex is not OperationCanceledException) { }
    }

    private async Task LoadHandlesAsync(GroupClient c, string gid, int version, int intent, CancellationToken token)
    {
        try
        {
            var members = await c.Api.MembersAsync(gid, token);
            if (!IsSelectionCurrent(c, version, intent, gid)) return;
            handles[gid] = members.Where(m => m.Handle is not null).ToDictionary(m => m.Address, m => m.Handle!, StringComparer.Ordinal);
        }
        catch (Exception ex) when (ex is not OperationCanceledException) { }
    }

    private void RebuildLines()
    {
        if (ActiveClient is not { } c || selected is not { Info.IsActive: true } row)
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
        if (!CanSend || ActiveClient is not { } c || selected is not { } row) return false;
        var version = generation;
        var intent = selectionGeneration;
        var token = scope.Token;
        var text = Draft.Trim();
        var files = DraftFiles.ToArray();
        IsSending = true;
        Error = null;
        try
        {
            await c.SendAsync(row.Id, text, files, token);
            if (!IsCurrent(c, version)) return false;
            if (IsSelectionCurrent(c, version, intent, row.Id)) { Draft = ""; DraftFiles.Clear(); RebuildLines(); }
            return true;
        }
        catch (OperationCanceledException) { return false; }
        catch (Exception ex) { if (IsCurrent(c, version)) Error = Describe(ex); return false; }
        finally { if (IsCurrent(c, version)) IsSending = false; }
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
        if (ActiveClient is not { } c || selected is not { Info.IsActive: true } row) return null;
        var version = generation; var intent = selectionGeneration; var token = scope.Token;
        try
        {
            var bytes = await c.OpenFileAsync(row.Id, file, token);
            return IsSelectionCurrent(c, version, intent, row.Id) ? bytes : null;
        }
        catch (OperationCanceledException) { return null; }
        catch (Exception ex) { if (IsSelectionCurrent(c, version, intent, row.Id)) Error = "Could not open this file. " + Describe(ex); return null; }
    }

    public async Task<GroupInfo?> CreateAsync(GroupSettings settings, IReadOnlyList<string> invite)
    {
        if (ActiveClient is not { } c) return null;
        var version = generation; var intent = selectionGeneration; var token = scope.Token;
        Error = null;
        try
        {
            var created = await c.CreateGroupAsync(settings, invite.Select(PostAddress.Canonical).ToArray(), token);
            if (!IsCurrent(c, version)) return null;
            var unresolved = created.Invitations?.Where(i => i.Status is not ("invited" or "already_member")).Select(i => i.Address).ToArray() ?? [];
            if (unresolved.Length > 0) Status = "Not invited: " + string.Join(", ", unresolved);
            await RefreshAsync();
            if (!IsCurrent(c, version)) return null;
            if (intent == selectionGeneration) await SelectAsync(Groups.FirstOrDefault(g => g.Id == created.Group.Id));
            return IsCurrent(c, version) ? created.Group : null;
        }
        catch (OperationCanceledException) { return null; }
        catch (Exception ex) { if (IsCurrent(c, version)) Error = Describe(ex); return null; }
    }

    public Task AnswerInvitationAsync(bool accept) => Act(async (c, row) =>
    {
        await c.Api.AnswerInvitationAsync(row.Id, accept, scope.Token);
        c.Api.RequireActive();
        Status = accept ? "Joined. A member's device adds you in a moment." : "Invitation declined.";
        if (!accept && selected?.Id == row.Id) selected = null;
    });

    public Task LeaveAsync() => Act(async (c, row) =>
    {
        await c.Api.LeaveAsync(row.Id, scope.Token);
        await c.ForgetAsync(row.Id);
        c.Api.RequireActive();
        if (selected?.Id == row.Id) selected = null;
        Status = "You left the group.";
    });

    public Task DeleteAsync() => Act(async (c, row) =>
    {
        await c.Api.DeleteAsync(row.Id, scope.Token);
        await c.ForgetAsync(row.Id);
        c.Api.RequireActive();
        if (selected?.Id == row.Id) selected = null;
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
        if (ActiveClient is not { } c || selected is not { } row) return;
        var version = generation;
        Error = null;
        try
        {
            await work(c, row);
            if (IsCurrent(c, version)) await RefreshAsync();
        }
        catch (OperationCanceledException) { }
        catch (Exception ex) { if (IsCurrent(c, version)) Error = Describe(ex); }
        if (IsCurrent(c, version)) NotifyAll();
    }

    // Group info: members with roles and their devices' fingerprints, and the group safety number.
    public async Task<(IReadOnlyList<GroupMemberRow> Members, string? SafetyNumber)> LoadInfoAsync()
    {
        if (ActiveClient is not { } c || selected is not { Info.IsActive: true } row) return ([], null);
        var version = generation; var intent = selectionGeneration; var token = scope.Token;
        try
        {
            var members = await c.Api.MembersAsync(row.Id, token);
            if (!IsSelectionCurrent(c, version, intent, row.Id)) return ([], null);
            handles[row.Id] = members.Where(m => m.Handle is not null).ToDictionary(m => m.Address, m => m.Handle!, StringComparer.Ordinal);
            var leaves = c.Members(row.Id);
            var rows = members.Select(m => new GroupMemberRow(m.Address, NameOf(m.Address), m.Role ?? "", m.State ?? "",
                    leaves.Where(l => l.Mailbox == m.Address).ToArray(), m.Address == mailbox?.Address, m.Revision ?? 0))
                .OrderBy(m => m.Role switch { "owner" => 0, "admin" => 1, _ => 2 }).ThenBy(m => m.Name, StringComparer.OrdinalIgnoreCase).ToArray();
            return (rows, c.SafetyNumberOf(row.Id));
        }
        catch (OperationCanceledException) { return ([], null); }
        catch (Exception ex) { if (IsSelectionCurrent(c, version, intent, row.Id)) Error = Describe(ex); return ([], null); }
    }

    // "Call in progress — Join" in the group header (calls service, same origin as the inbox).
    public async Task<GroupCallStatus?> CallStatusAsync(Uri callsOrigin)
    {
        if (ActiveClient is not { } c || selected is not { Info.IsActive: true } row || mailbox is null) return null;
        var version = generation; var intent = selectionGeneration; var token = scope.Token;
        try
        {
            using var actor = c.Api.BindActor();
            var validate = c.Api.CaptureValidation();
            var status = await postbox.GetCallsJsonAsync<GroupCallStatus>(new Uri(callsOrigin,
                "/calls/v1/groups/" + Uri.EscapeDataString(row.Id) + "/call?from=" + Uri.EscapeDataString(c.Mailbox)), token);
            validate();
            return IsSelectionCurrent(c, version, intent, row.Id) ? status : null;
        }
        catch (Exception) { return null; }
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
        GroupStateUnavailableException => "Saved group keys could not be opened. Try again before resetting this device.",
        Pigeonpost.GroupCrypto.GroupCryptoException.Failure f => "The group keys could not be updated (" + f.code + ").",
        HttpRequestException or TaskCanceledException => "Could not connect. Check your internet connection and try again.",
        _ => "Could not complete that action. Please try again.",
    };

    public void Dispose()
    {
        if (disposed) return;
        disposed = true;
        generation++; selectionGeneration++;
        var prior = client;
        client = null;
        prior?.Dispose();
        scope.Cancel();
        scope.Dispose();
        selected = null;
        DraftFiles.Clear();
        Draft = "";
        handles.Clear();
        Lines.Clear();
        Groups.Clear();
    }
}
