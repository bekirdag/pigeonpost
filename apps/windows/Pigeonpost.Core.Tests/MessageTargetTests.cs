using Pigeonpost.Core;

internal static class MessageTargetTests
{
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("A quoted parent opens its original subject within the same conversation", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            var target = vm.CaptureMessageTarget(vm.Messages.Single())!;
            Require(target.IsCurrent && vm.OpenReplyParent(target)?.Id == "parent_copy" && vm.SelectedSubject?.Id == "parent_subject");
            Require(!target.IsCurrent && target.IsAccountCurrent);
        });
        yield return ("A loaded message target cannot follow a same-mailbox reauthentication", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            var target = vm.CaptureMessageTarget(vm.Messages.Single())!;
            f.Root.Retire(); f.Root = new(); await vm.InitializeAsync();
            Require(!target.IsCurrent && !target.IsAccountCurrent && vm.OpenReplyParent(target) is null && vm.SelectedSubject?.Id == "reply_subject");
        });
        yield return ("Returning to the same subject does not revive its old message controls", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            var target = vm.CaptureMessageTarget(vm.Messages.Single())!;
            vm.SelectSubject(vm.Subjects.Single(s => s.Id == "parent_subject"));
            vm.SelectSubject(vm.Subjects.Single(s => s.Id == "reply_subject"));
            Require(!target.IsCurrent && vm.OpenReplyParent(target) is null);
        });
        yield return ("An unchanged refresh preserves the loaded message target but changed metadata retires it", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            var target = vm.CaptureMessageTarget(vm.Messages.Single())!;
            await vm.RefreshAsync(); Require(target.IsCurrent);
            f.Reply = f.Reply with { ReplyTo = f.Reply.ReplyTo! with { Quote = "Changed context" } };
            await vm.RefreshAsync(); Require(!target.IsCurrent && vm.OpenReplyParent(target) is null);
        });
        yield return ("Expired and missing quoted parents remain unavailable while the stored quote survives", async () =>
        {
            using var f = new Service(); f.Parent = f.Parent with { ExpiresAt = 1 };
            using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            var target = vm.CaptureMessageTarget(vm.Messages.Single())!;
            Require(vm.OpenReplyParent(target) is null && vm.Error?.Contains("unavailable", StringComparison.Ordinal) == true
                && vm.Messages.Single().ReplyQuote == "Stored fixture quote");
        });
        yield return ("Quoted parent navigation cannot leave the current conversation or use conflicting references", async () =>
        {
            using var f = new Service(); f.Reply = f.Reply with { ReplyTo = new(MessageId: "foreign_copy", Quote: "Stored fixture quote") };
            using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            Require(vm.OpenReplyParent(vm.CaptureMessageTarget(vm.Messages.Single())!) is null && vm.SelectedConversation?.Peer == Peer);
            f.Reply = f.Reply with { ReplyTo = new(MessageId: "parent_copy", MsgRef: "mref_bbbbbbbbbbbbbbbbbbbbbbbbbb", Quote: "Stored fixture quote") };
            await vm.RefreshAsync();
            Require(vm.OpenReplyParent(vm.CaptureMessageTarget(vm.Messages.Single())!) is null);
        });
        yield return ("Message targets reject a different model and an ambient foreign caller", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); using var other = new InboxViewModel(f);
            await vm.InitializeAsync(); await other.InitializeAsync();
            var target = vm.CaptureMessageTarget(vm.Messages.Single())!;
            Require(other.OpenReplyParent(target) is null);
            var foreign = new AuthorizationLease(); using var scope = new AuthorizationContext(foreign);
            Require(!target.IsCurrent && vm.OpenReplyParent(target) is null); foreign.Retire();
        });
        yield return ("Native receipt labels and edited markers remain accessible without inventing unknown status", () =>
        {
            var row = new ThreadMessage("copy", "Fixture", 100, null, true, Receipt: new("seen"), EditedAt: 110, EditCount: 1);
            Require(row.StatusLabel == "Seen" && row.ReceiptGlyph == "✓✓" && row.FooterTimeLabel.EndsWith(" · Edited", StringComparison.Ordinal));
            Require((row with { Receipt = null }).StatusLabel == "" && (row with { Receipt = new("unknown") }).ReceiptGlyph == "");
            Require((row with { IsOutgoing = false }).ReceiptGlyph == "" && (row with { Status = DeliveryStatus.Sending }).StatusLabel == "Sending…");
            return Task.CompletedTask;
        });
        yield return ("Unloading a message control retires its target while an explicit reload keeps the same accepted account", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            var target = vm.CaptureMessageTarget(vm.Messages.Single())!; target.Retire();
            Require(!target.IsCurrent && target.IsAccountCurrent && vm.OpenReplyParent(target) is null);
            Require(vm.CaptureMessageTarget(target.Message)?.IsCurrent == true);
        });
        yield return ("Switching inboxes and returning cannot revive a queued quoted-message action", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            var target = vm.CaptureMessageTarget(vm.Messages.Single())!;
            var original = vm.SelectedMailbox!;
            await vm.SwitchMailboxAsync(vm.Mailboxes.Single(m => m.Address != original.Address));
            await vm.SwitchMailboxAsync(original);
            Require(!target.IsCurrent && target.IsAccountCurrent && vm.OpenReplyParent(target) is null);
        });
        yield return ("A new UI action outlives its completed render task while a retired render callback remains refused", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            var render = f.Root.Child(); var resumed = new TaskCompletionSource(TaskCreationOptions.RunContinuationsAsynchronously);
            MessageTarget target; MessageViewBinding view; Task<bool> oldCallback;
            using (new AuthorizationContext(render))
            {
                target = vm.CaptureMessageTarget(vm.Messages.Single())!;
                view = vm.CaptureMessageView(vm.Messages.Single())!;
                async Task<bool> FromRender() { await resumed.Task; return target.IsCurrent || view.BeginAction() is not null; }
                oldCallback = FromRender();
            }
            render.Retire(); resumed.TrySetResult();
            Require(!await oldCallback && !target.IsCurrent && view.IsCurrent);
            Require(view.BeginAction() is { } action && vm.OpenReplyParent(action)?.Id == "parent_copy");
        });
        yield return ("Rendered message views cannot create actions for a reauthenticated account or newer selection", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            var view = vm.CaptureMessageView(vm.Messages.Single())!;
            f.Root.Retire(); f.Root = new(); await vm.InitializeAsync();
            Require(view.BeginAction() is null && !view.IsAccountCurrent);
            view = vm.CaptureMessageView(vm.Messages.Single())!;
            vm.SelectSubject(vm.Subjects.Single(s => s.Id == "parent_subject"));
            vm.SelectSubject(vm.Subjects.Single(s => s.Id == "reply_subject"));
            Require(view.BeginAction() is null && view.IsAccountCurrent);
        });
        yield return ("Unloaded rendered controls refuse new actions without retiring an already admitted action", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            var view = vm.CaptureMessageView(vm.Messages.Single())!; var action = view.BeginAction()!;
            view.Retire(); Require(view.BeginAction() is null && action.IsCurrent);
        });
    }
    private const string Own = "/k/target_fixture", Peer = "/bob/main";
    private static void Require(bool value) { if (!value) throw new Exception("Message target assertion failed."); }
    private sealed class Service : IInboxService, IDisposable
    {
        internal AuthorizationLease Root = new();
        public AuthorizationLease? AcceptedAuthorization => Root;
        internal InboxMessage Parent = new() { MessageId = "parent_copy", Body = "Parent fixture", From = "/k/bob", SenderHandle = Peer,
            ThreadId = "parent_subject", MsgRef = "mref_aaaaaaaaaaaaaaaaaaaaaaaaaa", ReceivedAt = 100, Read = true };
        internal InboxMessage Reply = new() { MessageId = "reply_copy", Body = "Reply fixture", From = "/k/bob", SenderHandle = Peer,
            ThreadId = "reply_subject", ReplyTo = new(MessageId: "parent_copy", Quote: "Stored fixture quote"), ReceivedAt = 200, Read = true };
        public Task<IReadOnlyList<Mailbox>> GetMailboxesAsync(CancellationToken ct) => Task.FromResult<IReadOnlyList<Mailbox>>([new(Own), new("/k/target_second")]);
        public Task<InboxSnapshot> LoadAsync(string identity, CancellationToken ct) => Task.FromResult(new InboxSnapshot(
            [Parent, Reply, new() { MessageId = "foreign_copy", Body = "Other conversation", From = "/carol/main", ReceivedAt = 50 }],
            [new("parent_subject", Peer), new("reply_subject", Peer)], [], new HashSet<string>()));
        public Task<SendReceipt> SendAsync(string identity, string peer, string body, string? threadId, CancellationToken ct) => throw new NotSupportedException();
        public Task<string> CreateThreadAsync(string identity, string peer, string title, CancellationToken ct) => throw new NotSupportedException();
        public Task SetArchivedAsync(string identity, string peer, bool archived, CancellationToken ct) => Task.CompletedTask;
        public Task AcknowledgeAsync(string identity, string messageId, CancellationToken ct) => Task.CompletedTask;
        public Task SetContactAsync(string identity, Contact contact, CancellationToken ct) => Task.CompletedTask;
        public Task RemoveContactAsync(string identity, string peer, CancellationToken ct) => Task.CompletedTask;
        public Task DeleteThreadAsync(string identity, string threadId, CancellationToken ct) => Task.CompletedTask;
        public void Dispose() => Root.Retire();
    }
}
