using Pigeonpost.Core;

internal static class MessageComposerTests
{
    public static IEnumerable<(string Name, Func<Task> Run)> All()
    {
        yield return ("Reply mode keeps the ordinary draft and files, and cancel removes only the reply context", async () =>
        {
            using var f = await Ready(); var vm = f.Model; vm.Draft = "Existing reply draft"; AddFile(vm);
            Require(vm.BeginReply(vm.CaptureMessageTarget(vm.Messages.Single(m => m.Id == "design-3"))!));
            Require(vm.HasComposerAction && !vm.IsEditingMessage && vm.Draft == "Existing reply draft" && vm.DraftAttachments.Count == 1 && vm.CanAttachFiles);
            Require(vm.CancelComposerAction() && !vm.HasComposerAction && vm.Draft == "Existing reply draft" && vm.DraftAttachments.Count == 1 && f.Service.Posts == 0);
        });
        yield return ("Edit mode restores the prior new-message draft and staged files on explicit cancel", async () =>
        {
            using var f = await Ready(); var vm = f.Model; vm.Draft = "Previous unsent draft"; AddFile(vm);
            var original = vm.Messages.Single(m => m.Id == "design-2");
            Require(vm.BeginEdit(vm.CaptureMessageTarget(original)!));
            Require(vm.IsEditingMessage && vm.Draft == original.DisplayBody && vm.DraftAttachments.Count == 0 && !vm.CanAttachFiles);
            AddFile(vm); Require(vm.DraftAttachments.Count == 0);
            vm.Draft = "Revised edit text";
            Require(vm.CancelComposerAction() && vm.Draft == "Previous unsent draft" && vm.DraftAttachments.Count == 1 && !vm.HasComposerAction);
        });
        yield return ("A protected reply freezes the original parent and quote and clears only its confirmed draft", async () =>
        {
            using var f = await Ready(); var vm = f.Model; var parent = vm.Messages.Single(m => m.Id == "design-3");
            Require(vm.BeginReply(vm.CaptureMessageTarget(parent)!)); vm.Draft = "Protected reply fixture";
            await vm.SendDraftAsync(); var sent = f.Service.Requests.Single();
            Require(sent.ReplyTo == parent.Id && sent.Quote == RequestEnvelope.ReplyQuote(parent.CopyText) && sent.EditOf is null
                && RequestEnvelope.DisplayText(sent.Body) == "Protected reply fixture" && vm.Draft == "" && !vm.HasComposerAction && !vm.IsSending);
        });
        yield return ("A confirmed protected edit keeps request semantics and restores the previous draft and files", async () =>
        {
            using var f = await Ready(); var vm = f.Model; vm.Draft = "Previous ordinary draft"; AddFile(vm);
            var original = vm.Messages.Single(m => m.Id == "design-2"); Require(vm.BeginEdit(vm.CaptureMessageTarget(original)!));
            vm.Draft = "Protected edited fixture"; await vm.SendDraftAsync(); var sent = f.Service.Requests.Single();
            Require(sent.EditOf == original.Id && sent.ReplyTo is null && sent.Attachments is null
                && sent.Body == RequestEnvelope.EditText(original.Body, "Protected edited fixture") && vm.Draft == "Previous ordinary draft"
                && vm.DraftAttachments.Count == 1 && !vm.HasComposerAction && !vm.IsSending);
        });
        yield return ("Reply/edit entry respects capability, ownership and the original 24-hour window", async () =>
        {
            var clock = new Clock(); using var f = await Ready(clock); var vm = f.Model;
            var incoming = vm.CaptureMessageTarget(vm.Messages.Single(m => m.Id == "design-3"))!;
            var own = vm.CaptureMessageTarget(vm.Messages.Single(m => m.Id == "design-2"))!;
            Require(!vm.BeginEdit(incoming)); vm.SetMessageFeatures(ServerFeatures.None);
            Require(!vm.BeginReply(incoming) && !vm.BeginEdit(own)); vm.SetMessageFeatures(Features()); clock.Advance(86401);
            Require(!vm.BeginEdit(own) && f.Service.Posts == 0);
        });
        yield return ("Leaving an edit selection restores its previous draft and does not revive the action on return", async () =>
        {
            using var f = await Ready(); var vm = f.Model; vm.Draft = "Selection backup"; AddFile(vm);
            Require(vm.BeginEdit(vm.CaptureMessageTarget(vm.Messages.Single(m => m.Id == "design-2"))!)); vm.Draft = "Cancelled edit contents";
            vm.SelectSubject(vm.Subjects.Single(s => s.Id == "design-release")); Require(!vm.HasComposerAction);
            vm.Draft = "Other subject draft"; vm.SelectSubject(vm.Subjects.Single(s => s.Id == "design-general"));
            Require(!vm.HasComposerAction && vm.Draft == "Selection backup" && vm.DraftAttachments.Count == 1 && f.Service.Posts == 0);
        });
        yield return ("An uncertain edit retains its action and explicitly recovers without consuming the previous new-message draft", async () =>
        {
            using var f = await Ready(); var vm = f.Model; vm.Draft = "Recovery backup"; AddFile(vm);
            Require(vm.BeginEdit(vm.CaptureMessageTarget(vm.Messages.Single(m => m.Id == "design-2"))!));
            vm.Draft = "Uncertain edited fixture"; f.Service.LoseNext = true; await vm.SendDraftAsync();
            Require(vm.IsEditingMessage && vm.Draft == "Uncertain edited fixture" && vm.HasPendingDeliveries && !vm.CancelComposerAction());
            var original = f.Service.Requests.Single(); Require(await vm.RetryPendingDeliveryAsync(vm.PendingDeliveries.Single().BeginAction()!));
            Require(f.Service.Posts == 2 && f.Service.Committed.Count == 1 && f.Service.Requests.All(r => r.MsgRef == original.MsgRef && r.EditOf == "design-2" && r.Body == original.Body)
                && vm.Draft == "Recovery backup" && vm.DraftAttachments.Count == 1 && !vm.HasComposerAction && !vm.HasPendingDeliveries && !vm.IsSending);
        });
        yield return ("Reply recovery cannot erase a later revision or its still-reviewed reply context", async () =>
        {
            using var f = await Ready(); var vm = f.Model; Require(vm.BeginReply(vm.CaptureMessageTarget(vm.Messages.Single(m => m.Id == "design-3"))!));
            vm.Draft = "Original reply contents"; f.Service.LoseNext = true; await vm.SendDraftAsync(); vm.Draft = "Later reply revision";
            Require(await vm.RetryPendingDeliveryAsync(vm.PendingDeliveries.Single().BeginAction()!));
            Require(vm.Draft == "Later reply revision" && vm.HasComposerAction && !vm.HasPendingDeliveries && f.Service.Committed.Count == 1);
        });
        yield return ("An edit that expires during fresh mailbox authorization cannot persist or POST", async () =>
        {
            var clock = new Clock(); using var f = await Ready(clock); var vm = f.Model;
            var parent = vm.Messages.Single(m => m.Id == "design-2"); Require(vm.BeginEdit(vm.CaptureMessageTarget(parent)!)); vm.Draft = "Expired edit contents";
            f.Service.BlockMailboxes = true; var send = vm.SendDraftAsync(); await f.Service.Waiting.Task.WaitAsync(TimeSpan.FromSeconds(3));
            clock.Advance(parent.At + 86401 - clock.Now); f.Service.Release.TrySetResult(); await send;
            Require(f.Service.Posts == 0 && vm.Draft == "Expired edit contents" && !vm.CanSendDraft && !vm.IsSending
                && !Directory.EnumerateFiles(f.Folder, "*.pending", SearchOption.AllDirectories).Any());
        });
        yield return ("An edit retains every original action caller through fresh asynchronous authorization", async () =>
        {
            using var f = await Ready(); var vm = f.Model; var caller = f.Service.Root.Child(); MessageTarget target;
            using (new AuthorizationContext(caller)) target = vm.CaptureMessageTarget(vm.Messages.Single(m => m.Id == "design-2"))!;
            Require(vm.BeginEdit(target)); vm.Draft = "Retired edit caller"; f.Service.BlockMailboxes = true;
            var send = vm.SendDraftAsync(); await f.Service.Waiting.Task.WaitAsync(TimeSpan.FromSeconds(3)); caller.Retire(); f.Service.Release.TrySetResult(); await send;
            Require(f.Service.Posts == 0 && f.Service.Root.IsActive && vm.Draft == "Retired edit caller" && !vm.IsSending && !vm.CanSendDraft);
        });
        yield return ("Changed original edit content is refused while its proposed text and previous draft remain recoverable", async () =>
        {
            using var f = await Ready(); var vm = f.Model; vm.Draft = "Original backup";
            Require(vm.BeginEdit(vm.CaptureMessageTarget(vm.Messages.Single(m => m.Id == "design-2"))!)); vm.Draft = "Proposed edit";
            f.Service.TransformLoad = snapshot => snapshot with { Messages = snapshot.Messages.Select(m => m.MessageId == "design-2" ? m with { Body = RequestEnvelope.Work("Other device edit"), EditCount = 2 } : m).ToArray() };
            await vm.RefreshAsync(); Require(!vm.CanSendDraft && vm.ComposerActionProblem.Contains("changed", StringComparison.Ordinal)); await vm.SendDraftAsync();
            Require(f.Service.Posts == 0 && vm.Draft == "Proposed edit" && vm.CancelComposerAction() && vm.Draft == "Original backup");
        });
        yield return ("Withdrawing the edit capability during authorization retires the pending action before POST", async () =>
        {
            using var f = await Ready(); var vm = f.Model; Require(vm.BeginEdit(vm.CaptureMessageTarget(vm.Messages.Single(m => m.Id == "design-2"))!)); vm.Draft = "Disabled edit fixture";
            f.Service.BlockMailboxes = true; var send = vm.SendDraftAsync(); await f.Service.Waiting.Task.WaitAsync(TimeSpan.FromSeconds(3));
            vm.SetMessageFeatures(ServerFeatures.None); f.Service.Release.TrySetResult(); await send;
            Require(f.Service.Posts == 0 && vm.Draft == "Disabled edit fixture" && !vm.IsSending && !vm.CanSendDraft);
        });
        yield return ("Leaving the original edit subject during authorization prevents its later persistence and POST", async () =>
        {
            using var f = await Ready(); var vm = f.Model; vm.Draft = "Selection authorization backup";
            Require(vm.BeginEdit(vm.CaptureMessageTarget(vm.Messages.Single(m => m.Id == "design-2"))!)); vm.Draft = "Obsolete selected edit";
            f.Service.BlockMailboxes = true; var send = vm.SendDraftAsync(); await f.Service.Waiting.Task.WaitAsync(TimeSpan.FromSeconds(3));
            vm.SelectSubject(vm.Subjects.Single(s => s.Id == "design-release")); f.Service.Release.TrySetResult(); await send;
            vm.SelectSubject(vm.Subjects.Single(s => s.Id == "design-general"));
            Require(f.Service.Posts == 0 && vm.Draft == "Selection authorization backup" && !vm.HasComposerAction && !vm.IsSending
                && !Directory.EnumerateFiles(f.Folder, "*.pending", SearchOption.AllDirectories).Any());
        });
        foreach (var deleted in new[] { false, true })
            yield return (deleted ? "Deleting the original reply parent during authorization prevents POST"
                : "An original edit changed on another device during authorization cannot be overwritten", async () =>
            {
                using var f = await Ready(); var vm = f.Model; var id = deleted ? "design-3" : "design-2";
                var target = vm.CaptureMessageTarget(vm.Messages.Single(m => m.Id == id))!;
                Require(deleted ? vm.BeginReply(target) : vm.BeginEdit(target)); vm.Draft = "Delayed stale action";
                f.Service.BlockMailboxes = true; var send = vm.SendDraftAsync(); await f.Service.Waiting.Task.WaitAsync(TimeSpan.FromSeconds(3));
                f.Service.TransformLoad = snapshot => snapshot with { Messages = deleted ? snapshot.Messages.Where(m => m.MessageId != id).ToArray()
                    : snapshot.Messages.Select(m => m.MessageId == id ? m with { Body = RequestEnvelope.Work("Other device version"), EditCount = 2 } : m).ToArray() };
                await vm.RefreshAsync(); f.Service.Release.TrySetResult(); await send;
                Require(f.Service.Posts == 0 && vm.Draft == "Delayed stale action" && !vm.CanSendDraft && !vm.IsSending
                    && !Directory.EnumerateFiles(f.Folder, "*.pending", SearchOption.AllDirectories).Any());
            });
        yield return ("A disappearing reply parent expires during authorization without persistence or POST", async () =>
        {
            var clock = new Clock(); using var f = await Ready(clock); var vm = f.Model;
            f.Service.TransformLoad = snapshot => snapshot with { Messages = snapshot.Messages.Select(m => m.MessageId == "design-3" ? m with { ExpiresAt = clock.Now + 30 } : m).ToArray() };
            await vm.RefreshAsync(); Require(vm.BeginReply(vm.CaptureMessageTarget(vm.Messages.Single(m => m.Id == "design-3"))!)); vm.Draft = "Expired reply fixture";
            f.Service.BlockMailboxes = true; var send = vm.SendDraftAsync(); await f.Service.Waiting.Task.WaitAsync(TimeSpan.FromSeconds(3));
            clock.Advance(30); f.Service.Release.TrySetResult(); await send;
            Require(f.Service.Posts == 0 && vm.Draft == "Expired reply fixture" && !vm.CanSendDraft && !vm.IsSending
                && !Directory.EnumerateFiles(f.Folder, "*.pending", SearchOption.AllDirectories).Any());
        });
        yield return ("An edit cannot adopt an unretired replacement account during authorization", async () =>
        {
            using var f = await Ready(); var vm = f.Model; Require(vm.BeginEdit(vm.CaptureMessageTarget(vm.Messages.Single(m => m.Id == "design-2"))!)); vm.Draft = "Old owner edit fixture";
            var original = f.Service.Root; f.Service.BlockMailboxes = true; var send = vm.SendDraftAsync(); await f.Service.Waiting.Task.WaitAsync(TimeSpan.FromSeconds(3));
            f.Service.Rotate(retire: false); await vm.InitializeAsync(); vm.Draft = "New account draft";
            f.Service.Release.TrySetResult(); await send;
            Require(original.IsActive && f.Service.Posts == 0 && !vm.IsSending && vm.Draft == "New account draft"
                && !Directory.EnumerateFiles(f.Folder, "*.pending", SearchOption.AllDirectories).Any());
            Require(!vm.HasComposerAction && !vm.HasPendingDeliveries); original.Retire();
        });
    }
    private static ServerFeatures Features() => new(new HashSet<string> { ServerFeatures.MessageEdit, ServerFeatures.MessageReply, ServerFeatures.Receipts });
    private static async Task<ProtectedComposerTests.Fixture> Ready(Clock? clock = null)
    { var f = await ProtectedComposerTests.Fixture.Create(clock ?? new Clock()); f.Model.SetMessageFeatures(Features()); return f; }
    private static void AddFile(InboxViewModel vm) => vm.AddAttachments([new("mode_file", "fixture.txt", 3, _ => Task.FromResult(new byte[] { 1, 2, 3 }))]);
    private static void Require(bool value) { if (!value) throw new Exception("Message composer action assertion failed."); }
    private sealed class Clock : TimeProvider
    {
        internal long Now = 1789113600;
        private readonly List<Timer> timers = [];
        public override DateTimeOffset GetUtcNow() => DateTimeOffset.FromUnixTimeSeconds(Now);
        public override ITimer CreateTimer(TimerCallback callback, object? state, TimeSpan dueTime, TimeSpan period)
        { var timer = new Timer(this, callback, state); timer.Change(dueTime, period); timers.Add(timer); return timer; }
        internal void Advance(long seconds)
        { Now += seconds; foreach (var timer in timers.ToArray()) timer.Fire(); }
        private sealed class Timer(Clock clock, TimerCallback callback, object? state) : ITimer
        {
            private bool disposed; private long due = long.MaxValue;
            public bool Change(TimeSpan dueTime, TimeSpan period)
            {
                if (disposed) return false;
                if (period != Timeout.InfiniteTimeSpan) throw new NotSupportedException("This fixture uses one-shot deadlines.");
                due = dueTime == Timeout.InfiniteTimeSpan ? long.MaxValue : clock.Now + (long)Math.Ceiling(dueTime.TotalSeconds); return true;
            }
            internal void Fire() { if (disposed || due > clock.Now) return; due = long.MaxValue; callback(state); }
            public void Dispose() => disposed = true;
            public ValueTask DisposeAsync() { Dispose(); return ValueTask.CompletedTask; }
        }
    }
}
