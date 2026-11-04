using Pigeonpost.Core;
using Pigeonpost.Core.Groups;
using Pigeonpost.Core.Preview;

// Windows group administration (audit 2026-11-03 P1-14 and P3-6): invite links, joining by link,
// who can join, members changing the timer, ownership transfer, ban and unban, and stepping down.
// The pure rules first, then the page model against the stand-in postbox with the real MLS library.
internal static class GroupAdminTests
{
    private sealed class Tokens : IAccessTokenProvider
    {
        public Task<string> GetTokenAsync(CancellationToken token) => Task.FromResult("test");
        public Task<string> RefreshTokenAsync(CancellationToken token) => Task.FromResult("test");
    }

    private static void Require(bool condition, string reason) { if (!condition) throw new Exception(reason); }

    private static GroupInfo Group(string role, string join = "private", int members = 3) => new()
    {
        Id = "grp_abc", Name = "Crew", JoinPolicy = join, ActivationState = "active", MemberCount = members,
        My = new GroupMy { Role = role, State = "active" },
    };
    private static GroupMemberRow Row(string role, string state = "active", bool me = false) =>
        new("/k/" + role + state + me, role + " person", role, state, [], me, 1);

    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Group admin rules: the menu offers what the postbox allows for each role", () =>
        {
            GroupMemberAction[] Of(string role, GroupMemberRow m) => GroupAdmin.ActionsFor(Group(role), m).ToArray();
            Require(Of("owner", Row("admin")).SequenceEqual([GroupMemberAction.RemoveAdmin, GroupMemberAction.Remove, GroupMemberAction.Ban]),
                "The owner may demote, remove and ban an admin.");
            Require(Of("admin", Row("admin")).Length == 0, "An admin cannot demote, remove or ban another admin (D-07).");
            Require(Of("admin", Row("member")).SequenceEqual([GroupMemberAction.MakeAdmin, GroupMemberAction.Restrict, GroupMemberAction.Remove, GroupMemberAction.Ban]),
                "An admin manages a member.");
            Require(Of("admin", Row("restricted")).SequenceEqual([GroupMemberAction.MakeAdmin, GroupMemberAction.Unrestrict, GroupMemberAction.Remove, GroupMemberAction.Ban]),
                "An admin lets a restricted member post again.");
            Require(Of("member", Row("member")).Length == 0 && Of("member", Row("member", "banned")).Length == 0, "A member manages nobody.");
            Require(Of("owner", Row("owner")).Length == 0, "Nobody changes the owner here.");
            Require(Of("admin", Row("admin", me: true)).SequenceEqual([GroupMemberAction.StepDown]), "An admin may step down.");
            Require(Of("owner", Row("owner", me: true)).Length == 0, "The owner transfers instead of stepping down.");
            Require(Of("member", Row("member", me: true)).Length == 0, "A member has nothing to step down from.");
            Require(Of("admin", Row("member", "banned")).SequenceEqual([GroupMemberAction.Unban]), "Admins unban.");
            Require(Of("admin", Row("member", "invited")).SequenceEqual([GroupMemberAction.RevokeInvitation]), "Admins revoke invitations.");
            Require(GroupAdmin.ActionsFor(Group("owner") with { ActivationState = "provisioning" }, Row("member")).Count == 0, "Nothing before the group is active.");
            Require(GroupAdmin.Change(GroupMemberAction.Ban) == (null, "banned") && GroupAdmin.Change(GroupMemberAction.StepDown) == ("member", null)
                && GroupAdmin.Change(GroupMemberAction.RevokeInvitation) == (null, "removed"), "Each action sends the postbox's role or state.");
            Require(GroupAdmin.Label(GroupMemberAction.Ban) == "Remove and ban" && GroupAdmin.Label(GroupMemberAction.StepDown) == "Step down as admin", "Labels match the other clients.");

            var members = new[] { Row("owner", me: true), Row("admin"), Row("admin", "keying"), Row("member") };
            Require(GroupAdmin.TransferCandidates(Group("owner"), members).Single().Role == "admin", "Ownership goes only to an active admin.");
            Require(GroupAdmin.TransferCandidates(Group("admin"), members).Count == 0, "Only the owner transfers.");
            Require(!GroupAdmin.MayLeave(Group("owner")) && GroupAdmin.MayLeave(Group("owner", members: 1)) && GroupAdmin.MayLeave(Group("admin")),
                "An owner with members transfers before leaving.");
            Require(GroupAdmin.ManagesLink(Group("admin", "public")) && !GroupAdmin.ManagesLink(Group("admin")) && !GroupAdmin.ManagesLink(Group("member", "public")),
                "Admins of public groups manage the link.");
            var restricted = Group("restricted") with { MembersCanChangeTimer = true };
            Require(!restricted.CanChangeTimer && (Group("member") with { MembersCanChangeTimer = true }).CanChangeTimer && !Group("member").CanChangeTimer,
                "Members change the timer only when allowed, and never while restricted.");
            return Task.CompletedTask;
        });

        yield return ("Invite links: only the postbox's own /g/ links parse", () =>
        {
            const string token = "AbCdEfGhIjKlMnOpQrStUvWxYz012345";
            Require(GroupAdmin.ParseInviteLink($"https://inbox.pigeonpost.dev/g/grp_abc123#{token}") == ("grp_abc123", token), "Link with a token.");
            Require(GroupAdmin.ParseInviteLink("  https://inbox.pigeonpost.dev/g/grp_abc123/ ") == ("grp_abc123", null), "Listed group without a token.");
            foreach (var bad in new[] { "", "grp_abc123", "http://inbox.pigeonpost.dev/g/grp_abc123#" + token, "https://evil.example/g/grp_abc123#" + token,
                         "https://inbox.pigeonpost.dev/g/abc#" + token, "https://inbox.pigeonpost.dev/g/grp_abc123#short", "https://inbox.pigeonpost.dev/g/grp_a b#" + token,
                         "https://inbox.pigeonpost.dev.evil.example/g/grp_abc123#" + token })
                Require(GroupAdmin.ParseInviteLink(bad) is null, "Refused: " + bad);
            var preview = new GroupPreview("grp_abc", "Crew", "Weekly sync", 4, "public", "unlisted", null, true, "join");
            var text = GroupAdmin.PreviewText(preview, "/jane");
            Require(text.StartsWith("Weekly sync", StringComparison.Ordinal) && text.Contains("4 members · joining as /jane.", StringComparison.Ordinal), "Preview text: " + text);
            Require(GroupAdmin.PreviewText(preview with { CanJoin = false, Needs = "invitation" }, "/jane").Contains("accept it from Invitations", StringComparison.Ordinal), "Invited people are sent to the invitation.");
            Require(GroupAdmin.PreviewText(preview with { CanJoin = false, MyState = "banned" }, "/jane").Contains("cannot join", StringComparison.Ordinal), "Banned people are told.");
            Require(GroupAdmin.ExplainLinkFailure(new PostboxException(404, "invalid_link")) == "This invite link is not valid any more.", "A reset link is explained.");
            return Task.CompletedTask;
        });

        yield return ("Groups page admin: link create/reset/off, join by link, join policy, timer permission, transfer, ban, unban and step down", async () =>
        {
            var server = new PreviewPostbox();
            var alice = server.AddMailbox(PreviewPostbox.Seed(91), "/alice/main");
            var bob = server.AddMailbox(PreviewPostbox.Seed(92), "/bob/main");
            var carol = server.AddMailbox(PreviewPostbox.Seed(93), "/carol/main");
            using var client = new PostboxClient(new HttpClient(server), new Tokens(), new Uri("http://127.0.0.1:9"));
            var features = await client.GetCapabilitiesAsync(default);
            using var a = new GroupsModel(client, _ => new MemoryGroupStateStore());
            using var b = new GroupsModel(client, _ => new MemoryGroupStateStore());
            using var c = new GroupsModel(client, _ => new MemoryGroupStateStore());
            await a.ActivateAsync(new Mailbox(alice, "/alice/main"), features, [new Mailbox(alice, "/alice/main")], []);
            await b.ActivateAsync(new Mailbox(bob, "/bob/main"), features, [new Mailbox(bob, "/bob/main")], []);
            await c.ActivateAsync(new Mailbox(carol, "/carol/main"), features, [new Mailbox(carol, "/carol/main")], []);

            var created = await a.CreateAsync(new GroupSettings("Open crew", JoinPolicy: "public"), ["/bob/main"]);
            Require(created is not null, "Created: " + a.Error);
            var gid = created!.Id;
            await b.RefreshAsync(); await b.SelectAsync(b.Groups.Single()); await b.AnswerInvitationAsync(true);
            await a.RefreshAsync(); await b.RefreshAsync();
            await a.SelectAsync(a.Groups.Single());
            Require(a.Info!.HasLink != true && GroupAdmin.ManagesLink(a.Info), "A public group starts without a link.");

            var first = await a.SetLinkAsync(true);
            var parsed = GroupAdmin.ParseInviteLink(first?.Url);
            Require(parsed is { } p && p.GroupId == gid && p.Token is not null && a.Info!.HasLink == true, "Create link returns a /g/ URL: " + first?.Url + " " + a.Error);
            var second = await a.SetLinkAsync(true);
            Require(second?.Url != first!.Url, "Reset makes a new link.");

            // Carol: the old link is refused, the new one previews and joins.
            await c.RefreshAsync();
            Require(await c.PreviewLinkAsync(gid, parsed!.Value.Token) is null && c.Error == "This invite link is not valid any more.", "A reset link stops working: " + c.Error);
            var token = GroupAdmin.ParseInviteLink(second!.Url)!.Value.Token;
            var preview = await c.PreviewLinkAsync(gid, token);
            Require(preview is { CanJoin: true, Name: "Open crew", MemberCount: 2 }, "The current link previews the group.");
            Require(await c.JoinByLinkAsync(gid, token) && c.Selected?.Id == gid && c.Selected.IsKeying, "Joining opens the group, waiting for keys: " + c.Error);
            await a.RefreshAsync(); await c.RefreshAsync();
            await c.SelectAsync(c.Groups.Single());
            Require(c.ShowsComposer, "Carol is keyed in by a member's device.");

            await a.SetLinkAsync(false);
            Require(a.Info!.HasLink != true && a.Status == "Invite link turned off.", "Turn off link.");
            await a.SetLinkAsync(true);
            await a.SetJoinPolicyAsync(false);
            Require(a.Info!.JoinPolicy == "private", "Who can join: invite only.");
            Require(await a.SetLinkAsync(true) is null && a.Error is not null, "A private group has no link.");

            await a.SetMembersCanChangeTimerAsync(true);
            await b.RefreshAsync(); await b.SelectAsync(b.Groups.Single());
            Require(a.Info!.MembersCanChangeTimer == true && b.Info!.CanChangeTimer, "Members may now change the timer.");
            await b.SetTimerAsync(86400);
            Require(b.Error is null && b.TimerBanner.Length > 0, "A member changes the timer: " + b.Error);
            await a.SetMembersCanChangeTimerAsync(false);
            await b.RefreshAsync(); await b.SelectAsync(b.Groups.Single());
            Require(!b.Info!.CanChangeTimer, "Only admins again.");

            // Ban Carol, then unban; she cannot rejoin while banned.
            var (members, _) = await a.LoadInfoAsync();
            var carolRow = members.Single(m => m.Address == carol);
            Require(GroupAdmin.ActionsFor(a.Info!, carolRow).Contains(GroupMemberAction.Ban), "The owner may ban.");
            await a.MemberActionAsync(carolRow, GroupMemberAction.Ban);
            (members, _) = await a.LoadInfoAsync();
            var banned = members.Single(m => m.Address == carol);
            Require(banned.State == "banned" && GroupAdmin.ActionsFor(a.Info!, banned).SequenceEqual([GroupMemberAction.Unban]) && a.Error is null, "Banned and listed for unban: " + a.Error);
            await a.SetJoinPolicyAsync(true);
            var again = (await a.SetLinkAsync(true))!.Url;
            await c.RefreshAsync();
            var t2 = GroupAdmin.ParseInviteLink(again)!.Value.Token;
            Require(await c.PreviewLinkAsync(gid, t2) is { CanJoin: false, MyState: "banned" }, "A banned person sees that they cannot join.");
            Require(!await c.JoinByLinkAsync(gid, t2) && c.Error!.Contains("cannot join", StringComparison.Ordinal), "A ban outlasts a valid link: " + c.Error);
            await a.MemberActionAsync(banned, GroupMemberAction.Unban);
            (members, _) = await a.LoadInfoAsync();
            Require(members.All(m => m.Address != carol) && a.Status!.Contains("no longer banned", StringComparison.Ordinal), "Unbanned: " + a.Status);
            Require(await c.JoinByLinkAsync(gid, t2), "After an unban the link works again: " + c.Error);

            // Ownership: Bob becomes admin, takes over; Alice then steps down.
            var bobRow = members.Single(m => m.Address == bob);
            await a.MemberActionAsync(bobRow, GroupMemberAction.MakeAdmin);
            (members, _) = await a.LoadInfoAsync();
            var heir = GroupAdmin.TransferCandidates(a.Info!, members).Single();
            Require(heir.Address == bob, "Bob is the only candidate.");
            await a.TransferOwnershipAsync(heir);
            Require(a.Error is null && a.Info!.Role == "admin" && !a.Info.IsOwner, "Alice is an admin after the transfer: " + a.Error);
            (members, _) = await a.LoadInfoAsync();
            Require(members.Single(m => m.Address == bob).Role == "owner", "Bob owns the group.");
            var me = members.Single(m => m.IsMe);
            Require(GroupAdmin.ActionsFor(a.Info!, me).SequenceEqual([GroupMemberAction.StepDown]), "Alice may step down.");
            await a.MemberActionAsync(me, GroupMemberAction.StepDown);
            Require(a.Error is null && a.Info!.Role == "member" && a.Status == "You are no longer an admin of this group.", "Stepped down: " + a.Error);
            Require(!a.Info.IsAdmin && GroupAdmin.MayLeave(a.Info), "A member may leave.");
        });
    }
}
