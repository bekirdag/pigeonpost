using Pigeonpost.Core;
using Pigeonpost.Core.Groups;

// §14C in groups: the pure rules every client shares (docs/message-actions.md §5). The MLS round
// trips, the postbox refusals and the page model are in GroupTests.
internal static class GroupActionsTests
{
    private static void Require(bool c, string reason) { if (!c) throw new Exception(reason); }
    private static void Equal<T>(T a, T e) => Require(EqualityComparer<T>.Default.Equals(a, e), $"Expected {e}, got {a}.");

    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Group references are positive decimal seqs exactly as written", () =>
        {
            Equal(GroupActions.Sequence("7"), 7L);
            Equal(GroupActions.Sequence("123"), 123L);
            foreach (var bad in new string?[] { null, "", "0", "-1", "07", "+5", "1.0", " 7", "7 ", "99999999999999999999" })
                Equal(GroupActions.Sequence(bad), null);
            return Task.CompletedTask;
        });

        yield return ("Group ticks: sending, sent until everyone listed it, seen only when every visible cursor read it", () =>
        {
            Equal(GroupActions.Receipt(new GroupLine { Pending = true }, null), "sending");
            var line = new GroupLine { Seq = 5 };
            Equal(GroupActions.Receipt(line, null), "sent");
            Equal(GroupActions.Receipt(line, new GroupReceipts([], true)), "sent");
            Equal(GroupActions.Receipt(line, new GroupReceipts([new("A", null, 5, 5), new("B", null, 4, null)], true)), "sent");
            Equal(GroupActions.Receipt(line, new GroupReceipts([new("A", null, 5, 5), new("B", null, 6, null)], true)), "seen");
            Equal(GroupActions.Receipt(line, new GroupReceipts([new("A", null, 5, 4), new("B", null, 5, 5)], true)), "delivered");
            Equal(GroupActions.Receipt(line, new GroupReceipts([new("A", null, 5, null), new("B", null, 5, null)], true)), "delivered");
            // With read receipts off for the caller, nobody is ever shown as having seen it.
            Equal(GroupActions.Receipt(line, new GroupReceipts([new("A", null, 5, 5)], false)), "delivered");
            Equal(GroupActions.ReceiptLabel("seen"), "Seen");
            return Task.CompletedTask;
        });

        yield return ("Group edits: own, readable, latest, unexpired, within 24 hours of the original", () =>
        {
            const string me = "/k/me";
            const long now = 1_000_000;
            GroupLine Line(long seq, long age, string text = "a", string sender = me, string? editOf = null) =>
                new() { Seq = seq, Sender = sender, CreatedAt = now - age, Text = text, EditOf = editOf, ActionsVerified = true };
            Require(GroupActions.Editable(Line(3, 100), [], me, now), "A recent own message is editable.");
            Require(!GroupActions.Editable(Line(3, 86_400), [], me, now), "The window closes at 24 hours.");
            Require(!GroupActions.Editable(Line(3, 100, sender: "/k/other"), [], me, now), "Someone else's message is not editable.");
            var deleted = Line(3, 100); deleted.Deleted = true;
            Require(!GroupActions.Editable(deleted, [], me, now), "A deleted message is not editable.");
            var undecryptable = Line(3, 100); undecryptable.Undecryptable = true;
            Require(!GroupActions.Editable(undecryptable, [], me, now), "An unreadable message is not editable.");
            var pending = Line(0, 1); pending.Pending = true;
            Require(!GroupActions.Editable(pending, [], me, now), "An unconfirmed send is not editable.");
            var expired = Line(3, 100); expired.ExpiresAt = now - 1;
            Require(!GroupActions.Editable(expired, [], me, now), "An expired message is not editable.");

            // The window belongs to the original: a fresh edit of an old message is closed.
            var oldRoot = Line(3, 90_000); var lateEdit = Line(5, 10, "b", editOf: "3");
            Require(!GroupActions.Editable(lateEdit, [oldRoot, lateEdit], me, now), "An edit inherits the original's 24 hours.");
            var root = Line(3, 100); var edit = Line(5, 10, "b", editOf: "3");
            Require(!GroupActions.Editable(root, [root, edit], me, now), "Only the latest version is edited.");
            Require(GroupActions.Editable(edit, [root, edit], me, now), "The latest version of a recent chain is editable.");
            // An edit that only lives on this device without its original cannot prove the window.
            Require(!GroupActions.Editable(edit, [edit], me, now), "A chain without its original is not editable.");
            // An edit inherits the original's timer.
            var timed = Line(3, 100); timed.ExpiresAt = now - 5; var later = Line(5, 10, "b", editOf: "3");
            Require(!GroupActions.Editable(later, [timed, later], me, now), "A version expires with its chain.");
            return Task.CompletedTask;
        });

        yield return ("Group versions fold only verified edits by the same sender", () =>
        {
            var root = new GroupLine { Seq = 3, Sender = "/k/me", Text = "a", CreatedAt = 10 };
            var edit = new GroupLine { Seq = 5, Sender = "/k/me", Text = "b", EditOf = "3", ActionsVerified = true, CreatedAt = 20 };
            Equal(string.Join(",", GroupActions.Versions(edit, [root, edit]).Select(v => v.Text)), "a,b");
            Require(GroupActions.Replaced(root, [root, edit]), "The original is replaced by its verified edit.");
            var unverified = new GroupLine { Seq = 5, Sender = "/k/me", Text = "b", EditOf = "3", ActionsVerified = false };
            Equal(GroupActions.Versions(unverified, [root, unverified]).Count, 1);
            Require(!GroupActions.Replaced(root, [root, unverified]), "An unverified reference replaces nothing.");
            var foreign = new GroupLine { Seq = 5, Sender = "/k/mallory", Text = "b", EditOf = "3", ActionsVerified = true };
            Require(!GroupActions.Replaced(root, [root, foreign]), "Another sender cannot replace a message.");
            var forward = new GroupLine { Seq = 2, Sender = "/k/me", Text = "b", EditOf = "3", ActionsVerified = true };
            Require(!GroupActions.Replaced(root, [root, forward]), "An edit names only an earlier version.");
            return Task.CompletedTask;
        });

        yield return ("Group replies quote the readable parent, or say it is unavailable", () =>
        {
            var parent = new GroupLine { Seq = 3, Sender = "/k/a", Text = "Question", CreatedAt = 10 };
            var reply = new GroupLine { Seq = 4, Sender = "/k/b", Text = "Answer", ReplyTo = "3", ActionsVerified = true };
            Require(ReferenceEquals(GroupActions.ReplyParent(reply, [parent, reply], 100), parent), "The parent is found.");
            parent.ExpiresAt = 50;
            Equal(GroupActions.ReplyParent(reply, [parent, reply], 100), null);
            Equal(GroupActions.ReplyParent(new GroupLine { Seq = 4, ReplyTo = "9" }, [parent], 0), null);
            return Task.CompletedTask;
        });

        yield return ("Refused group posts say what was refused, not that groups are off", () =>
        {
            Require(GroupActions.RefusalText(new PostboxException(403, "feature_disabled"), GroupComposerKind.Edit).Contains("Editing messages"), "Edit off.");
            Require(GroupActions.RefusalText(new PostboxException(403, "feature_disabled"), GroupComposerKind.Reply).Contains("Replying"), "Reply off.");
            Require(!GroupActions.RefusalText(new PostboxException(403, "feature_disabled"), null).Contains("Groups"), "A 403 is never groups off.");
            Require(!GroupActions.RefusalText(new PostboxException(403, "feature_disabled"), GroupComposerKind.Edit).Contains("Groups"), "An edit refusal is never groups off.");
            Require(GroupActions.RefusalText(new PostboxException(404, "feature_disabled"), null).Contains("Groups"), "Groups off is a 404.");
            Require(GroupActions.RefusalText(new PostboxException(403, "not_editable"), GroupComposerKind.Edit).Contains("24 hours"), "Window.");
            Require(GroupActions.RefusalText(new PostboxException(403, "posting_restricted"), null).Contains("Only admins"), "Announcement.");
            Require(GroupActions.RefusalText(new PostboxException(403, "org_read_only"), null).Contains("read-only"), "Org read-only.");
            Require(GroupActions.RefusalText(new PostboxException(409, "quota_exceeded"), null).Contains("out of storage"), "Quota.");
            Require(GroupActions.RefusalText(new PostboxException(403, "not_member"), null).Contains("not an active member"), "Membership.");
            Require(GroupActions.RefusalText(new PostboxException(429, "rate_limited", retryAfter: 3), null).Contains("3 seconds"), "Retry-After.");
            Require(GroupActions.RefusalText(new PostboxException(429, null), null).Contains("too fast"), "429 without a code.");
            return Task.CompletedTask;
        });
    }
}
