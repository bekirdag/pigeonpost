using Pigeonpost.Core;
using HeldUi = CallHistoryAuthorizationTests.HeldUi;

internal static class InboxAuthorizationTests
{
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Reauthentication to the same inbox discards the previous account's draft and refreshes its data", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            vm.Draft = "Old private draft"; vm.AddAttachments([File()]);
            f.Rotate(); f.Label = "replacement"; await vm.InitializeAsync();
            Require(vm.Draft == "" && vm.DraftAttachments.Count == 0 && vm.Messages.All(m => m.Body.Contains("replacement", StringComparison.Ordinal)));
            Require(f.Loads == 2 && vm.CanCompose);
        });
        yield return ("A verified same-person grant replacement keeps unsent text and files, including typing during verification", async () =>
        {
            using var f = new Service { Owner = "synthetic-person" }; using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            await vm.SwitchMailboxAsync(Beta); vm.Draft = "Beta draft";
            await vm.SwitchMailboxAsync(Alpha); vm.Draft = "Before verification"; vm.AddAttachments([File()]);
            Require(vm.ReadyForGrantReplacement);
            vm.Draft = "Typed while verifying"; // The browser step runs while the original grant is still accepted.
            var prior = f.Root; f.Rotate();
            Require(!vm.CarryUnsentWork(f.Root) && vm.CarryUnsentWork(prior));
            f.Label = "replacement"; await vm.InitializeAsync();
            Require(vm.SelectedMailbox?.Address == Alpha.Address && vm.Draft == "Typed while verifying" && vm.DraftAttachments.Count == 1 && vm.CanCompose);
            Require(vm.Messages.All(m => m.Body.Contains("replacement", StringComparison.Ordinal)) && f.Sends == 0);
            await vm.SwitchMailboxAsync(Beta); Require(vm.Draft == "Beta draft");
        });
        yield return ("Unsent work never follows another person, an unlisted inbox or an in-flight send", async () =>
        {
            using (var f = new Service { Owner = "synthetic-person" })
            {
                using var vm = new InboxViewModel(f); await vm.InitializeAsync(); vm.Draft = "Private";
                var prior = f.Root; f.Rotate(); f.Owner = "synthetic-other"; Require(vm.CarryUnsentWork(prior));
                await vm.InitializeAsync(); Require(vm.Draft == "" && vm.DraftAttachments.Count == 0);
            }
            using (var f = new Service { Owner = "synthetic-person" })
            {
                using var vm = new InboxViewModel(f); await vm.InitializeAsync(); await vm.SwitchMailboxAsync(Beta); vm.Draft = "Beta only";
                var prior = f.Root; f.Rotate(); f.Boxes = [Alpha]; Require(vm.CarryUnsentWork(prior)); await vm.InitializeAsync();
                Require(vm.SelectedMailbox?.Address == Alpha.Address && vm.Draft == "");
                f.Boxes = [Alpha, Beta]; await vm.InitializeAsync(); await vm.SwitchMailboxAsync(Beta); Require(vm.Draft == "");
            }
            using (var f = new Service { Owner = "synthetic-person" })
            {
                using var vm = new InboxViewModel(f); await vm.InitializeAsync(); vm.Draft = "Sending";
                var gate = f.Pause("send"); var sending = vm.SendDraftAsync(); await gate.Wait();
                Require(!vm.ReadyForGrantReplacement); gate.Release.TrySetResult(); await sending; Require(vm.ReadyForGrantReplacement);
                vm.Draft = "Dropped"; var prior = f.Root; f.Rotate(); Require(vm.CarryUnsentWork(prior)); vm.DropCarriedWork();
                await vm.InitializeAsync(); Require(vm.Draft == "");
            }
        });
        yield return ("A queued old initialization cannot replace the new account's mailbox list", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); var gate = f.Pause("mailboxes"); var ui = new HeldUi();
            var old = ui.Capture(vm.InitializeAsync); await gate.Wait(); gate.Release.TrySetResult(); await ui.Wait();
            f.Rotate(); f.Boxes = [Beta]; f.Label = "replacement"; await vm.InitializeAsync();
            ui.Resume(); await old;
            Require(vm.Mailboxes.Single().Address == Beta.Address && vm.SelectedMailbox?.Address == Beta.Address && !vm.IsBusy);
        });
        yield return ("A queued old refresh cannot refill a reauthenticated inbox", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            var gate = f.Pause("load"); var ui = new HeldUi();
            var old = ui.Capture(() => vm.RefreshAsync()); await gate.Wait(); gate.Release.TrySetResult(); await ui.Wait();
            f.Rotate(); f.Label = "replacement"; await vm.InitializeAsync();
            ui.Resume(); await old;
            Require(vm.Messages.All(m => m.Body.Contains("replacement", StringComparison.Ordinal)) && !vm.IsBusy);
        });
        yield return ("A queued old send cannot clear the replacement account's identical draft", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync(); vm.Draft = "same text";
            var gate = f.Pause("send"); var ui = new HeldUi();
            var old = ui.Capture(vm.SendDraftAsync); await gate.Wait(); gate.Release.TrySetResult(); await ui.Wait();
            f.Rotate(); f.Label = "replacement"; await vm.InitializeAsync(); vm.Draft = "same text";
            ui.Resume(); await old;
            Require(vm.Draft == "same text" && !vm.IsSending && vm.Error is null);
        });
        yield return ("A send cannot upload a file after its original accepted login has changed", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            var read = new Gate(); vm.Draft = "Private file";
            vm.AddAttachments([new DraftAttachment("file", "fixture.txt", 1, async _ =>
            {
                read.Started.TrySetResult(); await read.Release.Task.ConfigureAwait(false); return [1];
            })]);
            var old = vm.SendDraftAsync(); await read.Wait();
            f.Rotate(); f.Label = "replacement"; await vm.InitializeAsync();
            read.Release.TrySetResult(); await old;
            Require(f.Uploads == 0 && f.FileSends == 0 && vm.Draft == "" && !vm.IsSending);
        });
        yield return ("An inbox send keeps the same account across upload and delivery", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            vm.Draft = "Private file"; vm.AddAttachments([File()]); f.OnUpload = f.Rotate;
            await vm.SendDraftAsync();
            Require(f.Uploads == 1 && f.FileSends == 0);
        });
        yield return ("An accepted send cannot erase a later draft revision with the same text", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync(); vm.Draft = "same text";
            var gate = f.Pause("send"); var old = vm.SendDraftAsync(); await gate.Wait();
            vm.Draft = "new intent"; vm.Draft = "same text";
            gate.Release.TrySetResult(); await old;
            Require(vm.Draft == "same text" && f.Sends == 1 && !vm.IsSending);
        });
        yield return ("Old notification actions cannot initialize or borrow a replacement login", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            var loads = f.Loads; f.Rotate();
            Require(!await vm.ReplyFromAlertAsync(Alpha.Address, Peer, null, "Old reply"));
            Require(await vm.MarkConversationReadAsync(Alpha.Address, Peer) == 0 && f.Sends == 0 && f.Loads == loads && f.Acknowledgements == 0);
        });
        yield return ("Acknowledging a conversation cannot change accounts between messages", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            f.OnAcknowledge = f.Rotate;
            await vm.AcknowledgeSelectedAsync();
            Require(f.Acknowledgements == 1);
        });
        yield return ("An old notification read cannot acknowledge after its load resumed in a replacement inbox", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            var gate = f.Pause("load"); var ui = new HeldUi();
            var old = ui.Capture(() => vm.MarkConversationReadAsync(Alpha.Address, Peer));
            await gate.Wait(); gate.Release.TrySetResult(); await ui.Wait();
            f.Rotate(); f.Label = "replacement"; await vm.InitializeAsync();
            ui.Resume(); Require(await old == 0 && f.Acknowledgements == 0);
        });
        yield return ("Reinitializing the same accepted login preserves its current draft and file", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            vm.Draft = "Keep draft"; vm.AddAttachments([File()]);
            await vm.InitializeAsync();
            Require(vm.Draft == "Keep draft" && vm.DraftAttachments.Single().Id == "file" && vm.CanCompose);
        });
        yield return ("A completed thread creation preserves a newer subject selection even after leaving and returning", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            var selected = vm.SelectedSubject!.Id;
            var gate = f.Pause("create"); var old = vm.CreateSubjectAsync("New subject"); await gate.Wait();
            vm.SelectConversation(null); vm.SelectConversation(vm.Conversations.Single());
            gate.Release.TrySetResult(); Require(await old);
            Require(vm.SelectedSubject?.Id == selected);
        });
        yield return ("Inbox actions reject a replaced provider grant even when the previous grant was not retired", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            var previous = f.Root; f.Root = new AuthorizationLease();
            try
            {
                vm.Draft = "Old action";
                await vm.SendDraftAsync();
                Require(!await vm.ReplyFromAlertAsync(Alpha.Address, Peer, null, "Old reply"));
                Require(f.Sends == 0 && f.FileSends == 0 && vm.Draft == "");
            }
            finally { previous.Retire(); }
        });
        yield return ("A nested inbox caller cannot drop a retired outer caller constraint", async () =>
        {
            using var f = new Service(); using var vm = new InboxViewModel(f); await vm.InitializeAsync();
            var outer = f.Root.Child(); using var outerScope = new AuthorizationContext(outer);
            var inner = f.Root.Child(); using var innerScope = new AuthorizationContext(inner);
            outer.Retire();
            Require(!await vm.ReplyFromAlertAsync(Alpha.Address, Peer, null, "Old reply"));
            Require(await vm.MarkConversationReadAsync(Alpha.Address, Peer) == 0 && f.Sends == 0 && f.Loads == 1);
            inner.Retire();
        });
    }
    private static readonly Mailbox Alpha = new("/k/inbox_alpha", "/alpha/main");
    private static readonly Mailbox Beta = new("/k/inbox_beta", "/beta/main");
    private const string Peer = "/bob/main";
    private static DraftAttachment File() => new("file", "fixture.txt", 1, _ => Task.FromResult(new byte[] { 1 }));
    private static void Require(bool value) { if (!value) throw new Exception("Inbox ownership assertion failed."); }

    private sealed class Gate
    {
        internal readonly TaskCompletionSource Started = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal readonly TaskCompletionSource Release = new(TaskCreationOptions.RunContinuationsAsynchronously);
        internal Task Wait() => Started.Task.WaitAsync(TimeSpan.FromSeconds(3));
    }
    private sealed class Service : IInboxService, IAttachmentService, IConversationSettings, IDisposable
    {
        internal AuthorizationLease Root = new();
        public AuthorizationLease? AcceptedAuthorization => Root;
        internal string? Owner;
        public string? RecoveryOwnerKey => Owner;
        internal IReadOnlyList<Mailbox> Boxes = [Alpha, Beta];
        internal string Label = "original";
        internal int Loads, Sends, Uploads, FileSends, Acknowledgements;
        internal Action? OnUpload, OnAcknowledge;
        private bool createdThread;
        private Gate? gate;
        private string? pausedMethod;
        private int paused;
        internal Gate Pause(string method) { pausedMethod = method; return gate = new Gate(); }
        internal void Rotate() { Root.Retire(); Root = new AuthorizationLease(); }
        private async Task<T> Complete<T>(string method, T value)
        {
            if (gate is { } hold && method == pausedMethod && Interlocked.Exchange(ref paused, 1) == 0)
            {
                hold.Started.TrySetResult(); await hold.Release.Task.ConfigureAwait(false); // ignores cancellation
            }
            return value;
        }
        public Task<IReadOnlyList<Mailbox>> GetMailboxesAsync(CancellationToken ct) => Complete<IReadOnlyList<Mailbox>>("mailboxes", Boxes.ToArray());
        public Task<InboxSnapshot> LoadAsync(string identity, CancellationToken ct)
        {
            Loads++;
            var rows = Enumerable.Range(1, 2).Select(i => new InboxMessage
            {
                MessageId = "message_" + Label + "_" + i, From = "/k/fixture_peer", SenderHandle = Peer,
                Body = Label + " fixture " + i, ReceivedAt = 1_789_113_600 + i, Read = false,
            }).ToArray();
            ServerThread[] threads = createdThread ? [new("thread_fixture", Peer, "New subject", false)] : [];
            return Complete("load", new InboxSnapshot(rows, threads, [new Contact(Peer, "Bob", "allow", "review", [])], new HashSet<string>()));
        }
        public Task<SendReceipt> SendAsync(string identity, string peer, string body, string? threadId, CancellationToken ct)
        {
            Sends++; return Complete("send", new SendReceipt("recipient_fixture", "sender_fixture"));
        }
        public Task<MessageAttachment> UploadAsync(string identity, string filename, byte[] bytes, CancellationToken ct)
        {
            Uploads++; OnUpload?.Invoke(); return Complete("upload", new MessageAttachment("uploaded_fixture", filename, "text/plain", bytes.Length));
        }
        public Task<SendReceipt> SendAttachmentsAsync(string identity, string peer, string body, string? threadId, IReadOnlyList<string> ids, CancellationToken ct)
        {
            FileSends++; return Complete("send", new SendReceipt("recipient_fixture", "sender_fixture"));
        }
        public Task AcknowledgeAsync(string identity, string messageId, CancellationToken ct) { Acknowledgements++; OnAcknowledge?.Invoke(); return Task.CompletedTask; }
        public async Task<string> CreateThreadAsync(string identity, string peer, string title, CancellationToken ct)
        {
            var id = await Complete("create", "thread_fixture").ConfigureAwait(false); createdThread = true; return id;
        }
        public Task SetArchivedAsync(string identity, string peer, bool archived, CancellationToken ct) => Task.CompletedTask;
        public Task SetContactAsync(string identity, Contact contact, CancellationToken ct) => Task.CompletedTask;
        public Task RemoveContactAsync(string identity, string peer, CancellationToken ct) => Task.CompletedTask;
        public Task DeleteThreadAsync(string identity, string threadId, CancellationToken ct) => Task.CompletedTask;
        public Task SetThreadTimerAsync(string identity, string threadId, int? seconds, CancellationToken ct) => Task.CompletedTask;
        public Task ReportMessageAsync(string identity, string messageId, CancellationToken ct) => Task.CompletedTask;
        public void Dispose() { gate?.Release.TrySetResult(); Root.Retire(); }
    }
}
