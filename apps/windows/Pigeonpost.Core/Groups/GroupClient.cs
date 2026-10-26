using System.Text;
using System.Text.Json;
using Pigeonpost.GroupCrypto;

namespace Pigeonpost.Core.Groups;

// The group end-to-end encryption client every app runs against the postbox (docs/groups.md §3
// "Client loop"; protocol in docs/group-e2ee.md). A C# port of the web inbox's groups-core.js, so
// both behave the same way against the same server.
//
// What the postbox sees: who is in which group, which devices exist, ciphertext sizes and timing.
// What it cannot see: message text, filenames, attachment keys, group secrets. Everything readable
// is produced here by the MLS library and kept only in this device's own encrypted store — a
// device cannot decrypt a message twice, so the plaintext it decrypted once is the only copy.
//
// Everything that touches the MLS state runs one at a time (the library holds one pending commit
// per group) and the state is written after every mutation, before the next network request.
public sealed class GroupClient : IDisposable
{
    public const int KeyPackageTarget = 20;
    public const int KeyPackageFloor = 10;
    public const int HistoryLimit = 1000;
    public const int MaxRetries = 6;
    public const string DevicePrefix = "win-";

    private readonly GroupApi api;
    private readonly IAuthorizedGroupStateStore store;
    private readonly Func<long> now;
    private readonly Func<int, Task> sleep;
    private readonly Func<int> jitter;
    private readonly SemaphoreSlim gate = new(1, 1);
    private readonly object lifetime = new();
    private int users;
    private bool disposed;
    private bool gateDisposed;
    private GroupDevice? device;
    private GroupDeviceRecord? record;

    public GroupClient(GroupApi api, IGroupStateStore store, Func<long>? now = null, Func<int, Task>? sleep = null, Func<int>? jitter = null)
    {
        this.api = api;
        this.store = store as IAuthorizedGroupStateStore ?? throw new ArgumentException("Group storage must support authorization ownership.", nameof(store));
        this.now = now ?? (() => DateTimeOffset.UtcNow.ToUnixTimeSeconds());
        this.sleep = sleep ?? (ms => Task.Delay(ms));
        this.jitter = jitter ?? (() => Random.Shared.Next(0, 3000));
    }

    public string Mailbox => api.Mailbox;
    public GroupApi Api => api;
    public string? DeviceId => Local(() => record?.DeviceId);
    public bool Persistent => store.Persistent;
    public event Action<string>? HistoryChanged; // group id
    public event Action<string>? Removed;        // this device left the group's crypto

    // ---- device ---------------------------------------------------------------------------

    public Task<string> InitAsync(CancellationToken ct) => Serial(async () =>
    {
        // Authorize the exact canonical inbox before opening any cached plaintext.
        await api.VerifyMailboxAsync(ct).ConfigureAwait(false);
        var saved = await store.LoadAsync(Mailbox, api.Authorization).ConfigureAwait(false);
        api.RequireActive();
        if (saved is not null)
        {
            if (saved is not { Registered: true, StateBlob: { } blob }) throw new GroupStateUnavailableException();
            GroupDevice? restored = null;
            try
            {
                restored = Local(() => GroupDevice.ImportState(Convert.FromBase64String(blob), Convert.FromBase64String(saved.StateKey)));
                if (!Local(() => restored.Mailbox() == Mailbox && restored.DeviceId() == saved.DeviceId && restored.IsAttested()))
                    throw new GroupStateUnavailableException();
                var live = await api.DevicesAsync(null, ct).ConfigureAwait(false);
                var own = live.Devices?.SingleOrDefault(d => d.DeviceId == saved.DeviceId);
                if (live.Mailbox != Mailbox || own is null) throw new PostboxException(403, "device_revoked");
                if (!Local(() => own.SignatureKey == Convert.ToBase64String(restored.SignaturePublicKey())))
                    throw new GroupStateUnavailableException();
                Replace(restored);
                restored = null;
                record = saved;
            }
            catch (Exception ex) when (ex is not (OperationCanceledException or PostboxException)) { throw new GroupStateUnavailableException(); }
            finally { restored?.Dispose(); }
        }
        else await EnrolAsync(ct).ConfigureAwait(false);
        return record!.DeviceId;
    });

    private async Task EnrolAsync(CancellationToken ct)
    {
        var deviceId = DevicePrefix + GroupApi.RandomId(12);
        Replace(Local(() => new GroupDevice(Mailbox, deviceId)));
        record = new GroupDeviceRecord
        {
            DeviceId = deviceId,
            StateKey = Convert.ToBase64String(Pigeonpost.GroupCrypto.GroupCrypto.GenerateStateKey()),
        };
        var registered = await api.RegisterDeviceAsync(deviceId, UseDevice(d => d.SignaturePublicKey()), ct).ConfigureAwait(false);
        UseDevice(d => d.InstallAttestation(Convert.FromBase64String(registered.MailboxPublicKey), Convert.FromBase64String(registered.Attestation)));
        record.Registered = true;
        record.Fingerprint = registered.FingerprintDigits ?? Pigeonpost.GroupCrypto.GroupCrypto.FingerprintDigits(UseDevice(d => d.Fingerprint()));
        await PersistAsync().ConfigureAwait(false);
    }

    // A device the server no longer knows (revoked elsewhere, or a reset) starts again as new.
    public Task ReEnrolAsync(CancellationToken ct) => Serial(async () =>
    {
        await store.RemoveAsync(Mailbox, api.Authorization).ConfigureAwait(false);
        record = null;
        Replace(null);
        await EnrolAsync(ct).ConfigureAwait(false);
        return 0;
    });

    public Task<int> TopUpKeyPackagesAsync(CancellationToken ct) => Serial(async () =>
    {
        var count = await api.KeyPackageCountAsync(record!.DeviceId, ct).ConfigureAwait(false);
        if (count.Unclaimed >= KeyPackageFloor && count.LastResort) return 0;
        var need = Math.Max(0, KeyPackageTarget - count.Unclaimed);
        var packages = need > 0 ? UseDevice(d => d.GenerateKeyPackages((uint)need)) : [];
        var lastResort = count.LastResort ? null : UseDevice(d => d.GenerateLastResortKeyPackage());
        await PersistAsync().ConfigureAwait(false);
        await api.UploadKeyPackagesAsync(record.DeviceId, packages, lastResort, ct).ConfigureAwait(false);
        return packages.Length;
    });

    // ---- welcomes ---------------------------------------------------------------------------

    public async Task<IReadOnlyList<string>> ProcessWelcomesAsync(CancellationToken ct)
    {
        var joined = await Serial(async () =>
        {
            var list = new List<string>();
            foreach (var w in await api.WelcomesAsync(record!.DeviceId, ct).ConfigureAwait(false))
            {
                var ok = false;
                string? code = null;
                try
                {
                    UseDevice(d => { if (d.Groups().Contains(w.GroupId)) d.ForgetGroup(w.GroupId); });
                    UseDevice(d => d.JoinFromWelcome(Convert.FromBase64String(w.Welcome)));
                    var g = Cursor(w.GroupId);
                    g.InGroup = true;
                    g.Cursor = Math.Max(w.Seq ?? 0, 0);
                    g.Acked = 0;
                    g.Devices = DeviceMap(w.GroupId);
                    g.SafetyNumber = SafetyNumberOf(w.GroupId);
                    ok = true;
                    list.Add(w.GroupId);
                }
                catch (GroupCryptoException.Failure e) { code = e.code; }
                catch (Exception ex) when (ex is not OperationCanceledException) { code = "join_failed"; }
                await PersistAsync().ConfigureAwait(false);
                try { await api.AckWelcomeAsync(record.DeviceId, w.WelcomeId, ok, code, ct).ConfigureAwait(false); }
                catch (Exception ex) when (ex is not OperationCanceledException) { }
                if (ok)
                {
                    Remember(w.GroupId, new GroupLine { Kind = "notice", Seq = w.Seq ?? 0, Local = true, Notice = "you_joined", CreatedAt = now() });
                    await AckEpochAsync(w.GroupId, ct).ConfigureAwait(false);
                    await PersistAsync().ConfigureAwait(false);
                }
            }
            return list;
        }).ConfigureAwait(false);
        // A last-resort package may have been used for this join; a self-update replaces that leaf
        // key straight away, when the device had no single-use packages left (P-2 §13.6).
        if (joined.Count > 0)
        {
            KeyPackageCount? count = null;
            try { count = await api.KeyPackageCountAsync(record!.DeviceId, ct).ConfigureAwait(false); } catch (Exception ex) when (ex is not OperationCanceledException) { }
            if (count is { Unclaimed: 0 })
                foreach (var gid in joined)
                    try { await SelfUpdateAsync(gid, ct).ConfigureAwait(false); } catch (Exception ex) when (ex is not OperationCanceledException) { }
        }
        foreach (var gid in joined) HistoryChanged?.Invoke(gid);
        return joined;
    }

    private async Task AckEpochAsync(string gid, CancellationToken ct)
    {
        var epoch = UseDevice(d => d.Epoch(gid));
        var g = Cursor(gid);
        if (epoch <= g.Acked) return;
        try
        {
            await api.EpochAckAsync(gid, record!.DeviceId, epoch, ct).ConfigureAwait(false);
            g.Acked = epoch;
        }
        catch (Exception ex) when (ex is not OperationCanceledException) { /* retried after the next commit */ }
    }

    // ---- reading -----------------------------------------------------------------------------

    public Task<SyncResult> SyncAsync(string gid, CancellationToken ct) => Serial(() => SyncLockedAsync(gid, null, ct));

    private async Task<SyncResult> SyncLockedAsync(string gid, long? upTo, CancellationToken ct)
    {
        var g = Cursor(gid);
        if (!g.InGroup) return new(0);
        var processed = 0;
        for (var page = 0; page < 1000; page++)
        {
            GroupMessagesPage res;
            try { res = await api.MessagesAsync(gid, record!.DeviceId, g.Cursor, ct).ConfigureAwait(false); }
            catch (PostboxException e) when (e.StatusCode == 410)
            {
                await ResyncLockedAsync(gid, ct).ConfigureAwait(false);
                return new(processed, Resync: true);
            }
            var items = (res.Items ?? []).OrderBy(i => i.Seq).ToArray();
            var stopped = false;
            foreach (var item in items)
            {
                if (upTo is { } limit && item.Seq > limit) { stopped = true; break; }
                if (item.Seq <= g.Cursor) continue;
                var outcome = ProcessItem(gid, item);
                g.Cursor = item.Seq;
                processed++;
                await PersistAsync().ConfigureAwait(false);
                if (outcome == Outcome.Removed) { Removed?.Invoke(gid); HistoryChanged?.Invoke(gid); return new(processed, RemovedFromGroup: true); }
                if (outcome == Outcome.Gap) return new(processed, Gap: true);
                if (outcome == Outcome.Commit) { await AckEpochAsync(gid, ct).ConfigureAwait(false); await PersistAsync().ConfigureAwait(false); }
            }
            if (stopped || res.More != true || items.Length == 0) break;
        }
        if (processed > 0) HistoryChanged?.Invoke(gid);
        return new(processed);
    }

    private enum Outcome { Message, Commit, Notice, Removed, Gap, Skipped }

    private bool IsOwn(GroupItem item) => item.Sender == Mailbox && item.SenderDevice == record!.DeviceId;

    private Outcome ProcessItem(string gid, GroupItem item)
    {
        var created = item.CreatedAt ?? now();
        if (item.Type == "notice")
        {
            Remember(gid, new GroupLine
            {
                Kind = "notice", Seq = item.Seq, Notice = item.Kind, Actor = item.Actor, Metadata = Flatten(item.Metadata),
                CreatedAt = created, ExpiresAt = item.ExpiresAt,
            });
            return Outcome.Notice;
        }
        if (item.Type == "commit")
        {
            if (IsOwn(item) && !UseDevice(d => d.HasPendingCommit(gid))) return Outcome.Skipped; // merged when it was accepted
            Incoming result;
            try { result = UseDevice(d => d.ProcessIncoming(gid, Convert.FromBase64String(item.Commit ?? ""))); }
            catch (GroupCryptoException.Failure e) when (e.code == "future_epoch") { return Outcome.Gap; }
            catch (Exception e) when (e is not OperationCanceledException)
            {
                // A commit this device cannot verify: it stops at this epoch and fails closed (P-2 §3).
                Remember(gid, new GroupLine { Kind = "warning", Seq = item.Seq, Warning = "commit_rejected", Code = (e as GroupCryptoException.Failure)?.code ?? "mls", CreatedAt = created });
                return Outcome.Skipped;
            }
            CommitOutcome? o = result switch { Incoming.Commit c => c.Outcome, Incoming.OwnCommitConfirmed c => c.Outcome, _ => null };
            if (o is null) return Outcome.Skipped;
            RecordCommit(gid, item.Seq, o, created);
            if (!SameDevices(o.Added, item.Adds) || !SameDevices(o.Removed, item.Removes))
                Remember(gid, new GroupLine { Kind = "warning", Seq = item.Seq, Order = 1, Warning = "membership_mismatch", CreatedAt = created });
            if (o.SelfRemoved)
            {
                try { UseDevice(d => d.ForgetGroup(gid)); } catch (Exception ex) when (ex is not OperationCanceledException) { }
                Cursor(gid).InGroup = false;
                return Outcome.Removed;
            }
            return Outcome.Commit;
        }
        if (item.Type == "application")
        {
            if (item.DeletedAt is not null || item.Ciphertext is null)
            {
                Remember(gid, new GroupLine { Kind = "message", Seq = item.Seq, Sender = item.Sender, Deleted = true, CreatedAt = created, ExpiresAt = item.ExpiresAt, ReplacedBy = item.ReplacedBy });
                return Outcome.Message;
            }
            if (IsOwn(item))
            {
                // Recorded when it was sent; the server copy only confirms its seq.
                if (History(gid).FirstOrDefault(x => x.Kind == "message" && x.Cmid == item.ClientMessageId) is { } mine)
                {
                    mine.Seq = item.Seq; mine.CreatedAt = created; mine.ExpiresAt = item.ExpiresAt; mine.Pending = false;
                    Sort(gid);
                }
                return Outcome.Skipped;
            }
            Incoming result;
            try { result = UseDevice(d => d.ProcessIncoming(gid, Convert.FromBase64String(item.Ciphertext))); }
            catch (GroupCryptoException.Failure e) when (e.code == "future_epoch") { return Outcome.Gap; }
            catch (Exception e) when (e is not OperationCanceledException)
            {
                Remember(gid, new GroupLine
                {
                    Kind = "message", Seq = item.Seq, Sender = item.Sender, Undecryptable = true,
                    Code = (e as GroupCryptoException.Failure)?.code ?? "mls", CreatedAt = created, ExpiresAt = item.ExpiresAt,
                });
                return Outcome.Message;
            }
            if (result is not Incoming.Message { MessageValue: var m }) return Outcome.Skipped;
            if (m.Sender.Mailbox != item.Sender)
            {
                Remember(gid, new GroupLine { Kind = "warning", Seq = item.Seq, Warning = "sender_mismatch", CreatedAt = created });
                return Outcome.Skipped;
            }
            var doc = GroupBody.Decode(m.Body);
            var files = doc.Files.Select((f, i) => new LocalFile
            {
                Id = f.Id, Name = f.Name, Mime = f.Mime, Size = f.Size,
                Key = i < m.Attachments.Length ? Convert.ToBase64String(m.Attachments[i].Key) : null,
                Sha = i < m.Attachments.Length ? Convert.ToBase64String(m.Attachments[i].SealedSha256) : null,
                Len = i < m.Attachments.Length ? m.Attachments[i].PlaintextLen : 0,
            }).ToList();
            Remember(gid, new GroupLine
            {
                Kind = "message", Seq = item.Seq, Sender = item.Sender, SenderDevice = item.SenderDevice, Cmid = item.ClientMessageId,
                Text = doc.Text, Files = files, ReplyTo = doc.ReplyTo, EditOf = doc.EditOf ?? item.EditOf?.ToString(),
                CreatedAt = created, ExpiresAt = item.ExpiresAt,
            });
            if (item.EditOf is { } edited && History(gid).FirstOrDefault(x => x.Kind == "message" && x.Seq == edited) is { } target)
                target.ReplacedBy = item.Seq;
            return Outcome.Message;
        }
        return Outcome.Skipped;
    }

    // A verified commit, with what it means for the people in the group: a device added to a
    // mailbox that was already here is a device change the members must be shown (P-2 §12).
    private void RecordCommit(string gid, long seq, CommitOutcome o, long created)
    {
        var g = Cursor(gid);
        var before = g.Devices ?? [];
        var beforeMailboxes = before.Keys.Select(k => k.Split('|')[0]).ToHashSet(StringComparer.Ordinal);
        var line = new GroupLine
        {
            Kind = "commit", Seq = seq, Epoch = (long)o.Epoch, CreatedAt = created,
            Committer = Summary(o.Committer), Added = o.Added.Select(Summary).ToList(),
            Removed = o.Removed.Select(Summary).ToList(), Updated = o.Updated.Select(Summary).ToList(),
            NewDevices = o.Added.Where(a => beforeMailboxes.Contains(a.Mailbox)).Select(Summary).ToList(),
        };
        Remember(gid, line);
        if (o.SelfRemoved) return;
        g.Devices = DeviceMap(gid);
        var safety = SafetyNumberOf(gid);
        var changedDevices = o.Added.Any(a => beforeMailboxes.Contains(a.Mailbox))
            || o.Removed.Any(r => (g.Devices ?? []).Keys.Any(k => k.StartsWith(r.Mailbox + "|", StringComparison.Ordinal)))
            || o.Updated.Any(u => before.TryGetValue(u.Mailbox + "|" + u.DeviceId, out var fp) && fp != Hex(u.Fingerprint));
        if (changedDevices && g.SafetyNumber is not null && safety is not null && safety != g.SafetyNumber)
            Remember(gid, new GroupLine { Kind = "warning", Seq = seq, Order = 2, Warning = "safety_number_changed", CreatedAt = created });
        g.SafetyNumber = safety;
    }

    private static bool SameDevices(MemberInfo[] verified, IReadOnlyList<GroupDeviceRefJson>? declared)
    {
        if (declared is null) return true;
        var a = verified.Select(d => d.Mailbox + "|" + d.DeviceId).Order(StringComparer.Ordinal).ToArray();
        var b = declared.Select(d => d.Mailbox + "|" + d.DeviceId).Order(StringComparer.Ordinal).ToArray();
        return a.SequenceEqual(b);
    }

    private async Task ResyncLockedAsync(string gid, CancellationToken ct)
    {
        try { UseDevice(d => d.ForgetGroup(gid)); } catch (Exception ex) when (ex is not OperationCanceledException) { }
        var g = Cursor(gid);
        g.InGroup = false;
        Remember(gid, new GroupLine { Kind = "warning", Seq = g.Cursor, Order = 1, Warning = "resynced", CreatedAt = now() });
        await PersistAsync().ConfigureAwait(false);
        try { await api.ResyncAsync(gid, record!.DeviceId, ct).ConfigureAwait(false); } catch (Exception ex) when (ex is not OperationCanceledException) { }
        HistoryChanged?.Invoke(gid);
    }

    // ---- commits -----------------------------------------------------------------------------

    private sealed record PreparedCommit(CommitBundle Bundle, IReadOnlyList<CommitAdd> Adds, IReadOnlyList<GroupDeviceRefJson> Removes, long? Revision);

    // Build, post and merge one commit. On a lost race: discard, catch up, rebuild.
    private async Task<CommitAccepted?> CommitLockedAsync(string gid, Func<Task<PreparedCommit?>> plan, CancellationToken ct, bool activation = false)
    {
        for (var attempt = 0; attempt < MaxRetries; attempt++)
        {
            // A provisioning group has nothing to read yet (its creator is not an active reader).
            if (!activation) await SyncLockedAsync(gid, null, ct).ConfigureAwait(false);
            if (!Cursor(gid).InGroup) return null;
            var prepared = await plan().ConfigureAwait(false);
            if (prepared is null) return null;
            var bundle = prepared.Bundle;
            await PersistAsync().ConfigureAwait(false); // the pending commit is part of the state
            try
            {
                var res = await api.CommitAsync(gid, record!.DeviceId, new CommitBody(GroupApi.RequestKey(), bundle.Epoch, bundle.CommitId,
                    Convert.ToBase64String(bundle.Commit), Convert.ToBase64String(bundle.EnvelopeSignature),
                    bundle.Welcome is null ? null : Convert.ToBase64String(bundle.Welcome), prepared.Adds, prepared.Removes, prepared.Revision), ct).ConfigureAwait(false);
                // Anything sequenced before this commit is still in the old epoch: read it first.
                if (!activation) await SyncLockedAsync(gid, res.Seq - 1, ct).ConfigureAwait(false);
                var o = UseDevice(d => d.ConfirmCommit(gid));
                var g = Cursor(gid);
                g.Cursor = Math.Max(g.Cursor, res.Seq);
                RecordCommit(gid, res.Seq, o, now());
                await PersistAsync().ConfigureAwait(false);
                await AckEpochAsync(gid, ct).ConfigureAwait(false);
                await PersistAsync().ConfigureAwait(false);
                HistoryChanged?.Invoke(gid);
                return res;
            }
            catch (Exception e) when (e is not OperationCanceledException)
            {
                try { UseDevice(d => d.DiscardCommit(gid)); } catch (Exception ex) when (ex is not OperationCanceledException) { }
                await PersistAsync().ConfigureAwait(false);
                if (e is PostboxException { StatusCode: 409, Code: "revision_conflict" or "epoch_conflict" })
                {
                    await sleep(Math.Min(10000, 200 * (1 << attempt)) + Random.Shared.Next(0, 100)).ConfigureAwait(false);
                    continue;
                }
                throw;
            }
        }
        throw new PostboxException(409, "revision_conflict");
    }

    // Commit whatever the server says is pending: adds for keying members and new devices,
    // removals for departed members and revoked devices.
    public async Task<CommitAccepted?> ProcessPendingAsync(string gid, CancellationToken ct, bool noJitter = false)
    {
        var crypto = await api.CryptoAsync(gid, record!.DeviceId, ct).ConfigureAwait(false);
        if (crypto.InGroup != true && crypto.ActivationState == "active") return null;
        if (crypto.Pending is not { Count: > 0 }) return null;
        if (!noJitter)
        {
            await sleep(jitter()).ConfigureAwait(false);
            crypto = await api.CryptoAsync(gid, record.DeviceId, ct).ConfigureAwait(false);
            if (crypto.Pending is not { Count: > 0 }) return null;
        }
        return await Serial(() => CommitLockedAsync(gid, async () =>
        {
            var fresh = await api.CryptoAsync(gid, record.DeviceId, ct).ConfigureAwait(false);
            var pending = fresh.Pending ?? [];
            if (pending.Count == 0) return null;
            var addMailboxes = pending.Where(p => p.Kind == "add").Select(p => p.Mailbox).Distinct(StringComparer.Ordinal).ToArray();
            var removes = pending.Where(p => p.Kind == "remove").Select(p => new GroupDeviceRefJson(p.Mailbox, p.DeviceId)).ToArray();
            var claims = new List<KeyPackageClaim>();
            if (addMailboxes.Length > 0)
            {
                var claimed = await api.ClaimAsync(gid, record.DeviceId, addMailboxes, ct).ConfigureAwait(false);
                // Only devices the server still lists as pending; never this device itself.
                var wanted = pending.Where(p => p.Kind == "add").Select(p => p.Mailbox + "|" + p.DeviceId).ToHashSet(StringComparer.Ordinal);
                claims.AddRange((claimed.Claims ?? []).Where(c => wanted.Contains(c.Mailbox + "|" + c.DeviceId)
                    && !(c.Mailbox == Mailbox && c.DeviceId == record.DeviceId)));
            }
            if (claims.Count == 0 && removes.Length == 0) return null;
            var bundle = UseDevice(d => d.Commit(gid, claims.Select(c => Convert.FromBase64String(c.KeyPackage)).ToArray(),
                removes.Select(r => new DeviceRef(r.Mailbox, r.DeviceId)).ToArray()));
            return new PreparedCommit(bundle, claims.Select(c => new CommitAdd(c.Mailbox, c.DeviceId, c.KeyPackageRef)).ToArray(), removes, fresh.Revision);
        }, ct)).ConfigureAwait(false);
    }

    public Task<CommitAccepted?> SelfUpdateAsync(string gid, CancellationToken ct) => Serial(() => CommitLockedAsync(gid, async () =>
    {
        var fresh = await api.CryptoAsync(gid, record!.DeviceId, ct).ConfigureAwait(false);
        return new PreparedCommit(UseDevice(d => d.SelfUpdate(gid)), [], [], fresh.Revision);
    }, ct));

    // ---- creating ----------------------------------------------------------------------------

    // Create the row, create the MLS group at epoch 0, and post the first commit, which activates
    // the group. The mailbox's other devices, if any, are added in that same commit.
    public async Task<CreatedGroup> CreateGroupAsync(GroupSettings settings, IReadOnlyList<string> invite, CancellationToken ct)
    {
        var created = await api.CreateAsync(settings, invite, ct).ConfigureAwait(false);
        await ActivateAsync(created.Group.Id, ct).ConfigureAwait(false);
        return created;
    }

    public Task<CommitAccepted?> ActivateAsync(string gid, CancellationToken ct) => Serial(async () =>
    {
        if (!UseDevice(d => d.Groups().Contains(gid)))
        {
            UseDevice(d => d.CreateGroup(gid));
            var g = Cursor(gid);
            g.InGroup = true;
            g.Devices = DeviceMap(gid);
            g.SafetyNumber = SafetyNumberOf(gid);
            await PersistAsync().ConfigureAwait(false);
        }
        return await CommitLockedAsync(gid, async () =>
        {
            var fresh = await api.CryptoAsync(gid, record!.DeviceId, ct).ConfigureAwait(false);
            if (fresh.ActivationState == "active" && UseDevice(d => d.Epoch(gid)) > 0) return null;
            var adds = (fresh.Pending ?? []).Where(p => p.Kind == "add" && !(p.Mailbox == Mailbox && p.DeviceId == record.DeviceId)).ToArray();
            var claims = new List<KeyPackageClaim>();
            if (adds.Length > 0)
            {
                var claimed = await api.ClaimAsync(gid, record.DeviceId, adds.Select(a => a.Mailbox).Distinct(StringComparer.Ordinal).ToArray(), ct).ConfigureAwait(false);
                claims.AddRange((claimed.Claims ?? []).Where(c => !(c.Mailbox == Mailbox && c.DeviceId == record.DeviceId)));
            }
            var bundle = claims.Count > 0 ? UseDevice(d => d.Commit(gid, claims.Select(c => Convert.FromBase64String(c.KeyPackage)).ToArray(), [])) : UseDevice(d => d.SelfUpdate(gid));
            return new PreparedCommit(bundle, claims.Select(c => new CommitAdd(c.Mailbox, c.DeviceId, c.KeyPackageRef)).ToArray(), [], fresh.Revision);
        }, ct, activation: true).ConfigureAwait(false);
    });

    // ---- sending -----------------------------------------------------------------------------

    public sealed record OutgoingFile(string Name, string Mime, byte[] Bytes);

    public async Task<PostAccepted> SendAsync(string gid, string text, IReadOnlyList<OutgoingFile> files, CancellationToken ct, long? replyTo = null, long? editOf = null)
    {
        var cmid = "cm_" + GroupApi.RandomId(24);
        var uploaded = new List<GroupBody.File>();
        var refs = new List<AttachmentRef>();
        foreach (var f in files)
        {
            var sealedFile = Local(() => Pigeonpost.GroupCrypto.GroupCrypto.SealAttachment(f.Bytes));
            var res = await api.UploadAttachmentAsync(gid, record!.DeviceId, sealedFile.Sealed, ct).ConfigureAwait(false);
            uploaded.Add(new GroupBody.File(res.Id, f.Name, f.Mime, f.Bytes.Length));
            refs.Add(sealedFile.Reference);
        }
        var body = GroupBody.Encode(new GroupBody.Doc("text", text, uploaded, replyTo?.ToString(), editOf?.ToString()));
        var localFiles = uploaded.Select((f, i) => new LocalFile
        {
            Id = f.Id, Name = f.Name, Mime = f.Mime, Size = f.Size,
            Key = Convert.ToBase64String(refs[i].Key), Sha = Convert.ToBase64String(refs[i].SealedSha256), Len = refs[i].PlaintextLen,
        }).ToList();
        return await Serial(async () =>
        {
            var local = new GroupLine
            {
                Kind = "message", Pending = true, Sender = Mailbox, SenderDevice = record!.DeviceId, Cmid = cmid, Text = text, Files = localFiles,
                ReplyTo = replyTo?.ToString(), EditOf = editOf?.ToString(), CreatedAt = now(), Mine = true,
            };
            for (var attempt = 0; attempt < MaxRetries; attempt++)
            {
                await SyncLockedAsync(gid, null, ct).ConfigureAwait(false);
                if (!Cursor(gid).InGroup) throw new PostboxException(403, "not_member");
                var outgoing = UseDevice(d => d.EncryptMessage(gid, cmid, body, refs.ToArray()));
                await PersistAsync().ConfigureAwait(false);
                try
                {
                    var res = await api.PostAsync(gid, record.DeviceId, new PostBody("rk_" + cmid, cmid, outgoing.Epoch, Pigeonpost.GroupCrypto.GroupCrypto.EncScheme(),
                        Convert.ToBase64String(outgoing.Ciphertext), Convert.ToBase64String(outgoing.EnvelopeSignature),
                        uploaded.Select(f => f.Id).ToArray(), replyTo, editOf), ct).ConfigureAwait(false);
                    local.Seq = res.Seq;
                    local.Pending = false;
                    local.ExpiresAt = res.ExpiresAt;
                    Remember(gid, local);
                    if (editOf is { } edited && History(gid).FirstOrDefault(x => x.Kind == "message" && x.Seq == edited) is { } target) target.ReplacedBy = res.Seq;
                    await PersistAsync().ConfigureAwait(false);
                    HistoryChanged?.Invoke(gid);
                    return res;
                }
                catch (PostboxException e) when (e.StatusCode == 409 && e.Code == "epoch_conflict")
                {
                    // Catch up and re-encrypt under the same client message id.
                    await sleep(50 * (attempt + 1)).ConfigureAwait(false);
                }
                catch (PostboxException e) when (e.StatusCode == 409 && e.Code == "key_activation_pending")
                {
                    // A removal is waiting for a commit. Any member device may make it; this one does.
                    await CommitPendingRemovalsLockedAsync(gid, ct).ConfigureAwait(false);
                }
            }
            throw new PostboxException(409, "epoch_conflict");
        }).ConfigureAwait(false);
    }

    private Task<CommitAccepted?> CommitPendingRemovalsLockedAsync(string gid, CancellationToken ct) => CommitLockedAsync(gid, async () =>
    {
        var fresh = await api.CryptoAsync(gid, record!.DeviceId, ct).ConfigureAwait(false);
        var removes = (fresh.Pending ?? []).Where(p => p.Kind == "remove").Select(p => new GroupDeviceRefJson(p.Mailbox, p.DeviceId)).ToArray();
        if (removes.Length == 0) return null;
        return new PreparedCommit(UseDevice(d => d.Commit(gid, [], removes.Select(r => new DeviceRef(r.Mailbox, r.DeviceId)).ToArray())), [], removes, fresh.Revision);
    }, ct);

    public async Task<byte[]> OpenFileAsync(string gid, LocalFile file, CancellationToken ct)
    {
        if (file.Key is null || file.Sha is null) throw new InvalidOperationException("This file's key is not on this device.");
        var sealedBytes = await api.DownloadAttachmentAsync(gid, record!.DeviceId, file.Id, ct).ConfigureAwait(false);
        return Local(() => Pigeonpost.GroupCrypto.GroupCrypto.OpenAttachment(sealedBytes, new AttachmentRef(Convert.FromBase64String(file.Key), Convert.FromBase64String(file.Sha), file.Len)));
    }

    // ---- local housekeeping ------------------------------------------------------------------

    // Drop expired items by this device's own clock, whether or not the server's event arrived.
    public Task<int> PruneExpiredAsync() => Serial(async () =>
    {
        if (record is null) return 0;
        var t = now();
        var dropped = 0;
        foreach (var gid in record.History.Keys.ToArray())
        {
            var list = record.History[gid];
            dropped += list.RemoveAll(x => x.ExpiresAt is { } at && at <= t);
        }
        if (dropped > 0) await PersistAsync().ConfigureAwait(false);
        return dropped;
    });

    public Task DropSeqsAsync(string gid, IReadOnlyCollection<long> seqs) => Serial(async () =>
    {
        History(gid).RemoveAll(x => x.Kind == "message" && seqs.Contains(x.Seq));
        await PersistAsync().ConfigureAwait(false);
        return 0;
    });

    public Task ForgetAsync(string gid) => Serial(async () =>
    {
        try { UseDevice(d => d.ForgetGroup(gid)); } catch (Exception ex) when (ex is not OperationCanceledException) { }
        record?.Groups.Remove(gid);
        record?.History.Remove(gid);
        await PersistAsync().ConfigureAwait(false);
        return 0;
    });

    public bool InGroup(string gid) => Local(() => record?.Groups.TryGetValue(gid, out var g) == true && g.InGroup);
    public long CursorOf(string gid) => Local(() => record?.Groups.TryGetValue(gid, out var g) == true ? g.Cursor : 0);
    public IReadOnlyList<GroupLine> HistoryOf(string gid) => Local<IReadOnlyList<GroupLine>>(() => record?.History.TryGetValue(gid, out var list) == true ? list.ToArray() : []);
    public string? Fingerprint => Local(() => record?.Fingerprint);

    public IReadOnlyList<DeviceSummary> Members(string gid)
    {
        try { return Local(() => device?.Members(gid).Select(Summary).ToArray() ?? []); }
        catch (Exception ex) when (ex is not OperationCanceledException) { return []; }
    }

    public string? SafetyNumberOf(string gid)
    {
        try { return Local(() => device?.SafetyNumber(gid)); } catch (Exception ex) when (ex is not OperationCanceledException) { return null; }
    }

    public ulong? EpochOf(string gid)
    {
        try { return Local(() => device?.Epoch(gid)); } catch (Exception ex) when (ex is not OperationCanceledException) { return null; }
    }

    // ---- helpers -----------------------------------------------------------------------------

    private async Task<T> Serial<T>(Func<Task<T>> work)
    {
        api.RequireActive();
        lock (lifetime)
        {
            if (disposed) throw new AuthorizationRetiredException();
            users++;
        }
        var acquired = false;
        try
        {
            using var actor = api.BindActor();
            var validate = api.CaptureValidation();
            await gate.WaitAsync().ConfigureAwait(false);
            acquired = true;
            validate(); api.RequireActive();
            var value = await work().ConfigureAwait(false);
            validate(); api.RequireActive();
            return value;
        }
        finally
        {
            if (acquired) gate.Release();
            lock (lifetime)
            {
                users--;
                ReleaseGateIfIdle();
            }
        }
    }

    private T Local<T>(Func<T> work)
    {
        api.RequireActive();
        return api.Authorization.WithActive(work);
    }
    private T UseDevice<T>(Func<GroupDevice, T> work) => Local(() => work(device ?? throw new InvalidOperationException("Group encryption is not initialized.")));
    private void UseDevice(Action<GroupDevice> work) => UseDevice(d => { work(d); return 0; });

    private async Task PersistAsync()
    {
        record!.StateBlob = Convert.ToBase64String(UseDevice(d => d.ExportState(Convert.FromBase64String(record.StateKey))));
        await store.SaveAsync(Mailbox, record, api.Authorization).ConfigureAwait(false);
        api.RequireActive();
    }

    private GroupCursor Cursor(string gid)
    {
        if (!record!.Groups.TryGetValue(gid, out var g)) record.Groups[gid] = g = new GroupCursor();
        return g;
    }

    private List<GroupLine> History(string gid)
    {
        if (!record!.History.TryGetValue(gid, out var list)) record.History[gid] = list = [];
        return list;
    }

    private void Remember(string gid, GroupLine item)
    {
        var list = History(gid);
        var at = list.FindIndex(x => x.Seq == item.Seq && x.Kind == item.Kind && x.Order == item.Order && !x.Pending);
        if (at >= 0) list[at] = item;
        else list.Add(item);
        Sort(gid);
        if (list.Count > HistoryLimit) list.RemoveRange(0, list.Count - HistoryLimit);
    }

    private void Sort(string gid) => History(gid).Sort((a, b) =>
        a.Pending != b.Pending ? (a.Pending ? 1 : -1) : a.Seq != b.Seq ? a.Seq.CompareTo(b.Seq) : a.Order.CompareTo(b.Order));

    private Dictionary<string, string> DeviceMap(string gid) =>
        Members(gid).ToDictionary(m => m.Mailbox + "|" + m.DeviceId, m => m.Fingerprint, StringComparer.Ordinal);

    private static DeviceSummary Summary(MemberInfo m) => new(m.Mailbox, m.DeviceId, Hex(m.Fingerprint));

    // A device fingerprint (hex, as kept in history) in the digits people compare aloud.
    public static string Digits(string fingerprintHex)
    {
        try { return Pigeonpost.GroupCrypto.GroupCrypto.FingerprintDigits(Convert.FromHexString(fingerprintHex)); }
        catch (Exception) { return fingerprintHex; }
    }

    // Whether the group library can be used on this machine. Without it groups are hidden.
    public static bool LibraryAvailable()
    {
        try { return Pigeonpost.GroupCrypto.GroupCrypto.EncScheme().Length > 0; }
        catch (Exception ex) when (ex is DllNotFoundException or EntryPointNotFoundException or TypeInitializationException or BadImageFormatException) { return false; }
    }
    private static string Hex(byte[] bytes) => Convert.ToHexString(bytes).ToLowerInvariant();

    private static Dictionary<string, string>? Flatten(JsonElement? metadata)
    {
        if (metadata is not { ValueKind: JsonValueKind.Object } value) return null;
        var result = new Dictionary<string, string>(StringComparer.Ordinal);
        foreach (var p in value.EnumerateObject())
            result[p.Name] = p.Value.ValueKind == JsonValueKind.String ? p.Value.GetString() ?? "" : p.Value.GetRawText();
        return result;
    }

    private void Replace(GroupDevice? next)
    {
        try
        {
            Local(() =>
            {
                if (!ReferenceEquals(device, next)) device?.Dispose();
                device = next;
                return 0;
            });
        }
        catch { if (!ReferenceEquals(device, next)) next?.Dispose(); throw; }
    }

    public void Dispose()
    {
        api.Retire(); // waits for an admitted synchronous FFI operation before freeing its device
        lock (lifetime)
        {
            if (disposed) return;
            disposed = true;
            device?.Dispose();
            device = null;
            ReleaseGateIfIdle();
        }
    }
    private void ReleaseGateIfIdle()
    {
        if (!disposed || users != 0 || gateDisposed) return;
        record = null;
        gateDisposed = true;
        gate.Dispose();
    }
}

public sealed record SyncResult(int Processed, bool Resync = false, bool RemovedFromGroup = false, bool Gap = false);

// What travels inside the encryption. The postbox never sees any of it. Same format as the web.
public static class GroupBody
{
    public sealed record File(string Id, string Name, string Mime, long Size);
    public sealed record Doc(string Type, string Text, IReadOnlyList<File> Files, string? ReplyTo = null, string? EditOf = null);

    public static byte[] Encode(Doc doc)
    {
        var o = new Dictionary<string, object> { ["v"] = 1, ["type"] = string.IsNullOrEmpty(doc.Type) ? "text" : doc.Type };
        o["text"] = doc.Text.Length > 64000 ? doc.Text[..64000] : doc.Text;
        if (doc.Files.Count > 0)
            o["files"] = doc.Files.Select(f => new Dictionary<string, object>
            {
                ["id"] = f.Id, ["name"] = Trim(f.Name, 200, "file"), ["mime"] = Trim(f.Mime, 120, "application/octet-stream"), ["size"] = f.Size,
            }).ToArray();
        if (doc.ReplyTo is not null) o["reply_to"] = doc.ReplyTo;
        if (doc.EditOf is not null) o["edit_of"] = doc.EditOf;
        return JsonSerializer.SerializeToUtf8Bytes(o);
    }

    public static Doc Decode(byte[] bytes)
    {
        try
        {
            using var json = JsonDocument.Parse(bytes);
            var root = json.RootElement;
            if (root.ValueKind != JsonValueKind.Object) throw new JsonException();
            string? S(string name) => root.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.String ? v.GetString() : null;
            var files = new List<File>();
            if (root.TryGetProperty("files", out var list) && list.ValueKind == JsonValueKind.Array)
                foreach (var f in list.EnumerateArray().Where(f => f.ValueKind == JsonValueKind.Object).Take(32))
                {
                    string Field(string name, string fallback) => f.TryGetProperty(name, out var v) && v.ValueKind == JsonValueKind.String ? v.GetString() ?? fallback : fallback;
                    var size = f.TryGetProperty("size", out var s) && s.TryGetInt64(out var n) ? n : 0;
                    files.Add(new File(Field("id", ""), Field("name", "file"), Field("mime", "application/octet-stream"), size));
                }
            return new Doc(S("type") ?? "text", S("text") ?? "", files, S("reply_to"), S("edit_of"));
        }
        catch (JsonException) { return new Doc("text", Encoding.UTF8.GetString(bytes), []); }
    }

    private static string Trim(string value, int max, string fallback) => string.IsNullOrEmpty(value) ? fallback : value.Length > max ? value[..max] : value;
}
