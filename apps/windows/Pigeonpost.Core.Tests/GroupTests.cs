using System.Text;
using Pigeonpost.Core;
using Pigeonpost.Core.Groups;
using Pigeonpost.Core.Preview;
using Pigeonpost.GroupCrypto;
using GC = Pigeonpost.GroupCrypto.GroupCrypto;

// Phase 3b: the Windows group client against a stand-in postbox, with the real MLS library
// (pigeonpost_groupcrypto_ffi) doing every cryptographic step. docs/groups.md §3 "Client loop".
internal static class GroupTests
{
    private sealed class Tokens : IAccessTokenProvider
    {
        public Task<string> GetTokenAsync(CancellationToken token) => Task.FromResult("test");
        public Task<string> RefreshTokenAsync(CancellationToken token) => Task.FromResult("test");
    }

    private sealed class World : IDisposable
    {
        public readonly PreviewPostbox Server = new();
        public readonly PostboxClient Client;
        public long Now = 1_789_113_600;
        private readonly List<IDisposable> owned = [];
        public World()
        {
            Server.Clock = () => Now;
            Client = new PostboxClient(new HttpClient(Server), new Tokens(), new Uri("http://127.0.0.1:9"));
        }
        public GroupClient Device(string mailbox, IGroupStateStore? store = null)
        {
            var client = new GroupClient(new GroupApi(Client, mailbox), store ?? new MemoryGroupStateStore(), () => Now, _ => Task.CompletedTask, () => 0);
            owned.Add(client);
            return client;
        }
        public async Task<GroupClient> Ready(string mailbox, IGroupStateStore? store = null)
        {
            var d = Device(mailbox, store);
            await d.InitAsync(default);
            await d.TopUpKeyPackagesAsync(default);
            return d;
        }
        public void Dispose() { foreach (var d in owned) d.Dispose(); Client.Dispose(); }
    }

    private static void Require(bool condition, string reason) { if (!condition) throw new Exception(reason); }
    private static void Equal<T>(T actual, T expected) => Require(EqualityComparer<T>.Default.Equals(actual, expected), $"Expected {expected}, got {actual}.");
    private static async Task<PostboxException> Refused(Func<Task> action)
    {
        try { await action(); } catch (PostboxException ex) { return ex; }
        throw new Exception("Expected the postbox to refuse.");
    }
    private static IReadOnlyList<GroupLine> Messages(GroupClient c, string gid) => c.HistoryOf(gid).Where(l => l.Kind == "message").ToArray();
    private static bool Contains(byte[] haystack, string needle) => Encoding.UTF8.GetString(haystack).Contains(needle, StringComparison.Ordinal);

    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Group crypto library loads and the stand-in postbox derives real mailbox keys", () =>
        {
            Require(GC.EncScheme() == "mls-rfc9420-x25519-aes128gcm-ed25519+ppgc1", "Unexpected encryption scheme: " + GC.EncScheme());
            foreach (var fill in new byte[] { 1, 7, 42, 200 })
            {
                var seed = PreviewPostbox.Seed(fill);
                Equal(PreviewPostbox.AddressFor(seed), GC.MailboxForSeed(seed));
            }
            using var device = new GroupDevice(GC.MailboxForSeed(PreviewPostbox.Seed(3)), "win-test");
            Require(device.AttestationPayload().SequenceEqual(PreviewPostbox.AttestationPayload(device.Mailbox(), "win-test", device.SignaturePublicKey())),
                "Attestation payload layout differs from the library's.");
            Require(!device.IsAttested(), "A new device must start unattested.");
            return Task.CompletedTask;
        });

        yield return ("Message bodies round-trip and stay readable when a peer sends plain text", () =>
        {
            var doc = GroupBody.Decode(GroupBody.Encode(new GroupBody.Doc("text", "Hej ✓", [new("gat_1", "plan.pdf", "application/pdf", 12)], "4")));
            Equal(doc.Text, "Hej ✓"); Equal(doc.Files.Single().Name, "plan.pdf"); Equal(doc.ReplyTo, "4");
            Equal(GroupBody.Decode(Encoding.UTF8.GetBytes("not json")).Text, "not json");
            Equal(GroupBody.Decode(Encoding.UTF8.GetBytes("{\"v\":1,\"files\":[1,{\"id\":\"x\"}]}")).Files.Single().Id, "x");
            return Task.CompletedTask;
        });

        yield return ("Create, invite, accept, key, chat and share an encrypted file between devices", async () =>
        {
            using var w = new World();
            var alice = w.Server.AddMailbox(PreviewPostbox.Seed(11), "/alice/main");
            var bob = w.Server.AddMailbox(PreviewPostbox.Seed(12), "/bob/main");
            var carol = w.Server.AddMailbox(PreviewPostbox.Seed(13), "/carol/main");
            var a = await w.Ready(alice); var b = await w.Ready(bob); var c = await w.Ready(carol);
            Require(a.DeviceId!.StartsWith("win-", StringComparison.Ordinal), "Windows device ids start with win-.");
            Require(a.Fingerprint is { Length: > 10 }, "The device fingerprint is shown in digits.");

            var created = await a.CreateGroupAsync(new GroupSettings("Launch"), ["/bob/main", "/carol/main"], default);
            var gid = created.Group.Id;
            Equal(created.Invitations!.Count, 2);
            Require(a.InGroup(gid), "The creator's device is in the group after activation.");
            var bobView = (await b.Api.ListAsync(default)).Single();
            Require(bobView.IsInvited && bobView.Name == "Launch", "Bob sees the invitation once the group is active.");
            await b.Api.AnswerInvitationAsync(gid, true, default);
            await c.Api.AnswerInvitationAsync(gid, false, default);
            Equal((await c.Api.ListAsync(default)).Count, 0);

            var commit = await a.ProcessPendingAsync(gid, default, noJitter: true);
            Require(commit is not null, "Alice's device adds Bob's keying device.");
            Equal((await b.ProcessWelcomesAsync(default)).Single(), gid);
            Require((await b.Api.GetAsync(gid, default)).IsActive, "Bob becomes active once his device is in the tree.");

            await a.SendAsync(gid, "Hello Bob", [], default);
            await b.SyncAsync(gid, default);
            Equal(Messages(b, gid).Single().Text, "Hello Bob");
            Equal(Messages(b, gid).Single().Sender, alice);

            var file = Encoding.UTF8.GetBytes("Quarterly plan, confidential");
            await b.SendAsync(gid, "Plan attached", [new GroupClient.OutgoingFile("plan.txt", "text/plain", file)], default);
            await a.SyncAsync(gid, default);
            var received = Messages(a, gid).Last();
            Equal(received.Text, "Plan attached");
            var opened = await a.OpenFileAsync(gid, received.Files!.Single(), default);
            Require(opened.SequenceEqual(file), "The decrypted file differs from what Bob sent.");
            Equal(received.Files!.Single().Name, "plan.txt");

            // The postbox holds only ciphertext: no message text, filename or file content.
            foreach (var ct in w.Server.CiphertextsOf(gid))
            {
                var bytes = Convert.FromBase64String(ct);
                Require(!Contains(bytes, "Hello Bob") && !Contains(bytes, "plan.txt") && !Contains(bytes, "Plan attached"), "Plaintext reached the postbox.");
            }
            Require(w.Server.AttachmentBlobsOf(gid).All(blob => !Contains(blob, "Quarterly plan")), "File plaintext reached the postbox.");

            // Each side shows the same safety number and both devices.
            Require(a.SafetyNumberOf(gid) is { Length: > 0 } sn && sn == b.SafetyNumberOf(gid), "Safety numbers must match on both devices.");
            Equal(a.Members(gid).Count, 2);
        });

        yield return ("Group state is encrypted at rest and survives a restart", async () =>
        {
            using var w = new World();
            var alice = w.Server.AddMailbox(PreviewPostbox.Seed(21), "/alice/main");
            var bob = w.Server.AddMailbox(PreviewPostbox.Seed(22), "/bob/main");
            var folder = Path.Combine(Path.GetTempPath(), "pp-groups-" + Guid.NewGuid().ToString("N"));
            var key = System.Security.Cryptography.RandomNumberGenerator.GetBytes(32);
            try
            {
                var store = new FileGroupStateStore(folder, new FixedKeyProtector(key));
                var a = await w.Ready(alice, store);
                var b = await w.Ready(bob);
                var gid = (await a.CreateGroupAsync(new GroupSettings("Private"), ["/bob/main"], default)).Group.Id;
                await b.Api.AnswerInvitationAsync(gid, true, default);
                await a.ProcessPendingAsync(gid, default, noJitter: true);
                await b.ProcessWelcomesAsync(default);
                await b.SendAsync(gid, "Secret before restart", [], default);
                await a.SyncAsync(gid, default);
                var deviceId = a.DeviceId;
                a.Dispose();

                var file = Directory.GetFiles(folder).Single();
                var onDisk = File.ReadAllBytes(file);
                Require(!Contains(onDisk, "Secret before restart") && !Contains(onDisk, "state_blob") && !Contains(onDisk, alice), "The group store is readable on disk.");

                // Same store, new process: the device and its history come back; new messages decrypt.
                var again = w.Device(alice, new FileGroupStateStore(folder, new FixedKeyProtector(key)));
                Equal(await again.InitAsync(default), deviceId!);
                Equal(Messages(again, gid).Single().Text, "Secret before restart");
                await b.SendAsync(gid, "After restart", [], default);
                await again.SyncAsync(gid, default);
                Equal(Messages(again, gid).Last().Text, "After restart");

                // An unreadable store must stay available for recovery instead of being overwritten.
                var beforeRefusal = File.ReadAllBytes(file);
                var stranger = w.Device(alice, new FileGroupStateStore(folder, new FixedKeyProtector(new byte[32])));
                try { await stranger.InitAsync(default); throw new Exception("A foreign key must not open or reset the store."); }
                catch (GroupStateUnavailableException) { }
                Require(File.ReadAllBytes(file).SequenceEqual(beforeRefusal),
                    "Failed initialization must preserve the original encrypted device.");
            }
            finally { try { Directory.Delete(folder, true); } catch (IOException) { } }
        });

        yield return ("A second device joins without history and every member is shown the device change", async () =>
        {
            using var w = new World();
            var alice = w.Server.AddMailbox(PreviewPostbox.Seed(31), "/alice/main");
            var bob = w.Server.AddMailbox(PreviewPostbox.Seed(32), "/bob/main");
            var a = await w.Ready(alice); var b1 = await w.Ready(bob);
            var gid = (await a.CreateGroupAsync(new GroupSettings("Team"), ["/bob/main"], default)).Group.Id;
            await b1.Api.AnswerInvitationAsync(gid, true, default);
            await a.ProcessPendingAsync(gid, default, noJitter: true);
            await b1.ProcessWelcomesAsync(default);
            await a.SendAsync(gid, "Before the new laptop", [], default);
            await b1.SyncAsync(gid, default);
            var before = a.SafetyNumberOf(gid);

            var b2 = await w.Ready(bob); // Bob's new laptop
            // Both of Alice's and Bob's devices try to add it at once: one commit wins, the other
            // discards, catches up and finds nothing left to do.
            var race = await Task.WhenAll(a.ProcessPendingAsync(gid, default, noJitter: true), b1.ProcessPendingAsync(gid, default, noJitter: true));
            Equal(race.Count(r => r is not null), 1);
            Equal((await b2.ProcessWelcomesAsync(default)).Single(), gid);
            Require(Messages(b2, gid).Count == 0, "A new device gets no history.");
            await a.SendAsync(gid, "After the new laptop", [], default);
            await b2.SyncAsync(gid, default);
            await b1.SyncAsync(gid, default);
            Equal(Messages(b2, gid).Single().Text, "After the new laptop");
            Equal(Messages(b1, gid).Last().Text, "After the new laptop");

            await a.SyncAsync(gid, default);
            var change = a.HistoryOf(gid).Last(l => l.Kind == "commit" && l.Added is { Count: > 0 });
            Equal(change.Added!.Single().Mailbox, bob);
            Equal(change.Added!.Single().DeviceId, b2.DeviceId!);
            Require(a.HistoryOf(gid).Any(l => l.Warning == "safety_number_changed"), "A new device of an existing member changes the safety number visibly.");
            Require(a.SafetyNumberOf(gid) != before, "The safety number covers every device.");
            Equal(a.Members(gid).Count, 3);
        });

        yield return ("Removal, announcement mode and the epoch rules are enforced", async () =>
        {
            using var w = new World();
            var alice = w.Server.AddMailbox(PreviewPostbox.Seed(41), "/alice/main");
            var bob = w.Server.AddMailbox(PreviewPostbox.Seed(42), "/bob/main");
            var carol = w.Server.AddMailbox(PreviewPostbox.Seed(43), "/carol/main");
            var a = await w.Ready(alice); var b = await w.Ready(bob); var c = await w.Ready(carol);
            var gid = (await a.CreateGroupAsync(new GroupSettings("Ops"), ["/bob/main", "/carol/main"], default)).Group.Id;
            await b.Api.AnswerInvitationAsync(gid, true, default);
            await c.Api.AnswerInvitationAsync(gid, true, default);
            await a.ProcessPendingAsync(gid, default, noJitter: true);
            await b.ProcessWelcomesAsync(default); await c.ProcessWelcomesAsync(default);

            // Announcement mode: only admins post; the composer is replaced for others.
            var info = await a.Api.GetAsync(gid, default);
            info = await a.Api.UpdateAsync(gid, info.Revision!.Value, new Dictionary<string, object?> { ["post_policy"] = "admins" }, default);
            Require(info.AnnouncementOnly && info.CanPost, "The owner still posts in announcement mode.");
            var bobInfo = await b.Api.GetAsync(gid, default);
            Require(!bobInfo.CanPost, "A member cannot post in announcement mode.");
            Equal((await Refused(() => b.SendAsync(gid, "Can I?", [], default))).Code, "posting_restricted");
            info = await a.Api.UpdateAsync(gid, info.Revision!.Value, new Dictionary<string, object?> { ["post_policy"] = "everyone" }, default);
            Equal((await Refused(() => a.Api.UpdateAsync(gid, 1, new Dictionary<string, object?> { ["name"] = "Stale" }, default))).Code, "revision_conflict");

            // Removing Carol stops her access at once; her leaf is removed by the next commit and
            // sends wait for it (key_activation_pending is resolved by the sender's own device).
            var carolRow = (await a.Api.MembersAsync(gid, default)).Single(m => m.Address == carol);
            await a.Api.UpdateMemberAsync(gid, carol, carolRow.Revision!.Value, null, "removed", default);
            var epoch = w.Server.EpochOf(gid);
            await b.SendAsync(gid, "Carol has left", [], default);
            Require(w.Server.EpochOf(gid) == epoch + 1, "Bob's device committed the pending removal before posting.");
            Equal((await Refused(() => c.SyncAsync(gid, default))).Code, "not_member");
            await a.SyncAsync(gid, default);
            Equal(Messages(a, gid).Last().Text, "Carol has left");
            Require(a.Members(gid).All(m => m.Mailbox != carol), "Carol's device left the tree.");
        });

        yield return ("Disappearing messages leave by the device clock and the server stops serving them", async () =>
        {
            using var w = new World();
            var alice = w.Server.AddMailbox(PreviewPostbox.Seed(51), "/alice/main");
            var bob = w.Server.AddMailbox(PreviewPostbox.Seed(52), "/bob/main");
            var a = await w.Ready(alice); var b = await w.Ready(bob);
            var gid = (await a.CreateGroupAsync(new GroupSettings("Brief", EphemeralSecs: 300), ["/bob/main"], default)).Group.Id;
            await b.Api.AnswerInvitationAsync(gid, true, default);
            await a.ProcessPendingAsync(gid, default, noJitter: true);
            await b.ProcessWelcomesAsync(default);
            var sent = await a.SendAsync(gid, "Gone in five minutes", [], default);
            Equal(sent.ExpiresAt, w.Now + 300);
            await b.SyncAsync(gid, default);
            Equal(Messages(b, gid).Single().ExpiresAt, w.Now + 300);
            w.Now += 301;
            Equal(await b.PruneExpiredAsync(), 1);
            Equal(await a.PruneExpiredAsync(), 1);
            Require(Messages(b, gid).Count == 0 && Messages(a, gid).Count == 0, "Expired messages stay on screen.");
            Equal(Timers.Label(300), "5 minutes"); Equal(Timers.Label(null), "Off"); Equal(Timers.Label(2419200), "4 weeks");
            Equal(Timers.Choices.Count, 7);
        });

        yield return ("Groups need the groups flag, and a revoked device is refused", async () =>
        {
            using var w = new World();
            var alice = w.Server.AddMailbox(PreviewPostbox.Seed(61), "/alice/main");
            var a = await w.Ready(alice);
            Require((await w.Client.GetCapabilitiesAsync(default)).Has(ServerFeatures.Groups), "Capabilities list groups.");
            w.Server.RevokeDevice(alice, a.DeviceId!);
            Equal((await Refused(() => a.TopUpKeyPackagesAsync(default))).Code, "device_revoked");
            await a.ReEnrolAsync(default);
            Require(a.DeviceId != null && await a.TopUpKeyPackagesAsync(default) == GroupClient.KeyPackageTarget, "A re-enrolled device uploads fresh key packages.");
            w.Server.Features.Remove(ServerFeatures.Groups);
            Require(!(await w.Client.GetCapabilitiesAsync(default)).Has(ServerFeatures.Groups), "Capabilities follow the flag.");
            Equal((await Refused(() => a.Api.ListAsync(default))).Code, "feature_disabled");
        });
    }
}

// The Groups page model over the same stand-in postbox: rows, invitations, words for notices and
// device changes, announcement mode and the composer.
internal static class GroupsModelTests
{
    private sealed class Tokens : IAccessTokenProvider
    {
        public Task<string> GetTokenAsync(CancellationToken token) => Task.FromResult("test");
        public Task<string> RefreshTokenAsync(CancellationToken token) => Task.FromResult("test");
    }
    private static void Require(bool condition, string reason) { if (!condition) throw new Exception(reason); }

    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Groups page: invitation row, accept, keying, chat lines with names, announcement bar and info", async () =>
        {
            var server = new PreviewPostbox();
            var alice = server.AddMailbox(PreviewPostbox.Seed(71), "/alice/main");
            var bob = server.AddMailbox(PreviewPostbox.Seed(72), "/bob/main");
            using var client = new PostboxClient(new HttpClient(server), new Tokens(), new Uri("http://127.0.0.1:9"));
            var features = await client.GetCapabilitiesAsync(default);
            using var a = new GroupsModel(client, _ => new MemoryGroupStateStore());
            using var b = new GroupsModel(client, _ => new MemoryGroupStateStore());
            await a.ActivateAsync(new Mailbox(alice, "/alice/main"), features, [new Mailbox(alice, "/alice/main")], []);
            await b.ActivateAsync(new Mailbox(bob, "/bob/main"), features, [new Mailbox(bob, "/bob/main")], [new Contact("/alice/main", "Alice A.", "allow", "review")]);
            Require(a.Enabled && b.Enabled && a.DeviceFingerprint is not null, "Groups enabled with a device fingerprint.");
            Require(a.EmptyText.Length > 0, "Empty list explains how to start.");

            var created = await a.CreateAsync(new GroupSettings("Design crit"), ["/bob/main", "/nobody/here"]);
            Require(created is not null && a.Selected?.Id == created.Id && a.Status == "Not invited: /nobody/here", "Create selects the group and reports who was not invited: " + a.Status);
            await b.RefreshAsync();
            var invitation = b.Groups.Single();
            Require(invitation.IsInvitation && b.Invitations == 1 && invitation.Detail.StartsWith("Invitation", StringComparison.Ordinal), "Invitation row at the top.");
            await b.SelectAsync(invitation);
            Require(b.ShowsInvitation && !b.ShowsComposer, "An invitation shows Accept/Decline, not a composer.");
            await b.AnswerInvitationAsync(true);
            Require(b.Groups.Single().IsKeying && b.ShowsKeying, "Accepted invitation waits for keys.");
            await a.RefreshAsync(); // Alice's device adds Bob's
            await b.RefreshAsync(); // Bob's device joins from the welcome
            await b.SelectAsync(b.Groups.Single());
            Require(b.ShowsComposer && !b.ShowsKeying, "Bob can post once keyed.");

            a.Draft = "Morning! Draft is up.";
            Require(a.CanSend && await a.SendAsync(), "Alice sends.");
            await b.RefreshAsync();
            var line = b.Lines.Last(l => l.IsMessage);
            Require(line.Text == "Morning! Draft is up." && line.Sender == "Alice A." && !line.Mine, "Sender shown by the name Bob knows: " + line.Sender);
            Require(b.Lines.Any(l => l.Text == "You joined the group") && b.Groups.Single().Preview.StartsWith("Alice:", StringComparison.Ordinal),
                "Join notice and preview with the sender's short name: " + b.Groups.Single().Preview);

            await a.SetPostPolicyAsync(adminsOnly: true);
            await b.RefreshAsync();
            await b.SelectAsync(b.Groups.Single());
            Require(b.ShowsAnnouncementBar && !b.ShowsComposer && b.AnnouncementText == "Only admins can send messages", "Announcement mode replaces the composer.");
            Require(b.Lines.Any(l => l.Text == "Posting set to admins only"), "Settings notice reads as a sentence.");

            await a.SetTimerAsync(3600);
            await a.RefreshAsync();
            await a.SelectAsync(a.Groups.Single());
            Require(a.TimerBanner == "Messages disappear after 1 hour" && a.Lines.Any(l => l.Text == "You set disappearing messages to 1 hour"), "Timer banner and notice.");

            var (members, safety) = await a.LoadInfoAsync();
            Require(members.Count == 2 && members[0].RoleLabel == "Owner" && members.All(m => m.Devices.Count == 1), "Members with roles and devices.");
            Require(safety is { Length: > 0 } && members.All(m => m.DevicesLabel.Contains(" · ", StringComparison.Ordinal)), "Safety number and device fingerprints.");

            // Bob blocks Alice: her content is hidden for him, not removed for anyone else.
            b.SetContacts([new Contact("/alice/main", "Alice A.", "block", "review")]);
            await b.SelectAsync(b.Groups.Single());
            Require(b.Lines.Where(l => l.IsMessage).All(l => l.Text == "Hidden: you blocked this sender."), "Blocked sender's messages are hidden.");
        });
    }
}
