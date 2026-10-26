using System.Collections.ObjectModel;

namespace Pigeonpost.Core;

// The Calls page (SDS §12.1): newest first, All · Missed, a collapsed Filtered section, and the
// unseen-missed badge that viewing the page clears. Construct on the UI context.
public sealed class CallHistoryModel(PostboxClient postbox) : ObservableObject
{
    private string? identity;
    private string filter = "all";
    private long? next;
    private int unseen;
    private bool loading;
    private string? error;
    private int version;
    private CancellationTokenSource scope = new();
    private sealed class Actor(AuthorizationLease? accepted)
    {
        internal readonly AuthorizationLease? Accepted = accepted;
        internal readonly AuthorizationLease Authorization = accepted?.Child() ?? new AuthorizationLease();
    }
    private Actor? actor;
    private Actor? ActiveActor => actor is { } value && value.Authorization.IsActive
        && ReferenceEquals(value.Accepted, postbox.AcceptedAuthorization) ? value : null;
    private bool IsCurrent(Actor value, int generation) => generation == version && ReferenceEquals(ActiveActor, value);
    private IDisposable Bind(Actor value)
    {
        if (!ReferenceEquals(ActiveActor, value)) throw new AuthorizationRetiredException();
        return AuthorizationContext.BindActor(value.Accepted, value.Authorization);
    }

    public ObservableCollection<CallLogEntry> Entries { get; } = [];
    public ObservableCollection<CallLogEntry> FilteredEntries { get; } = [];
    public bool Loaded { get; private set; }
    public bool CanLoadMore => next is not null && !loading;
    public bool IsLoading { get => loading; private set { if (Set(ref loading, value)) Changed(nameof(CanLoadMore)); } }
    public string? Error { get => error; private set { if (Set(ref error, value)) Changed(nameof(HasError)); } }
    public bool HasError => Error is not null;
    public int UnseenMissed
    {
        get => unseen;
        private set { if (Set(ref unseen, Math.Max(0, value))) { Changed(nameof(HasBadge)); Changed(nameof(BadgeLabel)); Changed(nameof(AccessibleName)); } }
    }
    public bool HasBadge => UnseenMissed > 0;
    public string BadgeLabel => UnseenMissed > 99 ? "99+" : UnseenMissed.ToString();
    public string AccessibleName => UnseenMissed > 0 ? $"Calls, {UnseenMissed} missed" : "Calls";
    public string EmptyText => !Loaded || Entries.Count > 0 ? "" : filter == "missed" ? "No missed calls." : "No calls yet.";
    public string FilteredTitle => FilteredEntries.Count > 0 ? $"Filtered calls ({FilteredEntries.Count})" : "Filtered calls";
    public event EventHandler? EntriesChanged;

    public string Filter
    {
        get => filter;
        set
        {
            var wanted = value == "missed" ? "missed" : "all";
            if (!Set(ref filter, wanted)) return;
            _ = LoadAsync();
        }
    }

    // A new mailbox starts from nothing; the previous mailbox's requests are abandoned.
    public void Reset(string? mailbox)
    {
        version++;
        var prior = actor;
        actor = null;
        prior?.Authorization.Retire();
        scope.Cancel();
        scope.Dispose();
        scope = new();
        identity = mailbox;
        if (mailbox is not null && postbox.AcceptedAuthorization is not { IsActive: false })
        {
            try { actor = new Actor(postbox.AcceptedAuthorization); } catch (AuthorizationRetiredException) { }
        }
        next = null;
        IsLoading = false;
        Loaded = false;
        Error = null;
        Entries.Clear();
        FilteredEntries.Clear();
        UnseenMissed = 0;
        Changed(nameof(EmptyText));
        Changed(nameof(FilteredTitle));
        Changed(nameof(CanLoadMore));
        EntriesChanged?.Invoke(this, EventArgs.Empty);
    }

    public async Task LoadAsync(bool more = false)
    {
        if (identity is not { } mailbox || ActiveActor is not { } owner) return;
        var generation = ++version;
        var token = scope.Token;
        var wanted = filter;
        IsLoading = true;
        try
        {
            using var operation = Bind(owner);
            var validate = postbox.CaptureAuthorizationValidation();
            var page = await postbox.GetCallLogAsync(mailbox, wanted, more ? next : null, token);
            validate();
            if (!IsCurrent(owner, generation)) return;
            var rows = (page.Entries ?? []).Where(e => !string.IsNullOrEmpty(e.Id) && !e.IsFiltered).ToList();
            if (more)
            {
                var seen = Entries.Select(e => e.Id).ToHashSet(StringComparer.Ordinal);
                foreach (var row in rows.Where(r => seen.Add(r.Id))) Entries.Add(row);
            }
            else StableList.Reconcile(Entries, rows, e => e.Id, (a, b) => a == b);
            next = page.NextBefore;
            UnseenMissed = page.UnseenMissed ?? 0;
            Loaded = true;
            Error = null;
            if (!more)
            {
                var filtered = await postbox.GetCallLogAsync(mailbox, "filtered", null, token);
                validate();
                if (!IsCurrent(owner, generation)) return;
                StableList.Reconcile(FilteredEntries, (filtered.Entries ?? []).Where(e => !string.IsNullOrEmpty(e.Id)).ToArray(), e => e.Id, (a, b) => a == b);
            }
        }
        catch (OperationCanceledException) { }
        catch (PostboxException ex) when (ex.Code == "feature_disabled") { if (IsCurrent(owner, generation)) { Loaded = true; Error = null; } }
        catch (Exception) { if (IsCurrent(owner, generation)) Error = "Could not load your calls."; }
        finally
        {
            if (IsCurrent(owner, generation))
            {
                IsLoading = false;
                Changed(nameof(EmptyText));
                Changed(nameof(FilteredTitle));
                Changed(nameof(CanLoadMore));
                EntriesChanged?.Invoke(this, EventArgs.Empty);
            }
        }
    }

    // Looking at the page is seeing the missed calls on it.
    public async Task MarkSeenAsync()
    {
        if (identity is not { } mailbox || ActiveActor is not { } owner || UnseenMissed == 0 || Entries.Count == 0) return;
        var newest = Entries.Max(e => e.StartedAt);
        if (newest <= 0) return;
        var generation = version;
        var token = scope.Token;
        try
        {
            using var operation = Bind(owner);
            var validate = postbox.CaptureAuthorizationValidation();
            UnseenMissed = 0;
            var left = await postbox.MarkCallsSeenAsync(mailbox, newest, token);
            validate();
            if (IsCurrent(owner, generation)) UnseenMissed = left;
        }
        catch (Exception) { /* The badge returns with the next load. */ }
    }

    public async Task<string> DeleteAsync(CallLogEntry entry)
    {
        if (identity is not { } mailbox || ActiveActor is not { } owner) return "";
        var generation = version; var token = scope.Token;
        try
        {
            using var operation = Bind(owner);
            var validate = postbox.CaptureAuthorizationValidation();
            await postbox.DeleteCallAsync(mailbox, entry.Id, token);
            validate();
            if (!IsCurrent(owner, generation)) return "";
            Remove(entry.Id);
            return "Removed from call history.";
        }
        catch (OperationCanceledException) { return ""; }
        catch (Exception) { return IsCurrent(owner, generation) ? "Could not remove that call." : ""; }
    }

    public async Task<string> ReportAsync(CallLogEntry entry)
    {
        if (identity is not { } mailbox || ActiveActor is not { } owner) return "";
        var generation = version; var token = scope.Token;
        try
        {
            using var operation = Bind(owner);
            var validate = postbox.CaptureAuthorizationValidation();
            await postbox.ReportCallAsync(mailbox, entry.Id, token);
            validate();
            return IsCurrent(owner, generation) ? "Reported as spam." : "";
        }
        catch (OperationCanceledException) { return ""; }
        catch (Exception) { return IsCurrent(owner, generation) ? "Could not send the report." : ""; }
    }

    public async Task<string> ClearAsync()
    {
        if (identity is not { } mailbox || ActiveActor is not { } owner) return "";
        var generation = version; var token = scope.Token;
        try
        {
            using var operation = Bind(owner);
            var validate = postbox.CaptureAuthorizationValidation();
            await postbox.ClearCallsAsync(mailbox, token);
            validate();
            if (!IsCurrent(owner, generation)) return "";
            Entries.Clear();
            FilteredEntries.Clear();
            next = null;
            UnseenMissed = 0;
            Changed(nameof(EmptyText));
            Changed(nameof(FilteredTitle));
            EntriesChanged?.Invoke(this, EventArgs.Empty);
            return "Call history cleared.";
        }
        catch (OperationCanceledException) { return ""; }
        catch (Exception) { return IsCurrent(owner, generation) ? "Could not clear your call history." : ""; }
    }

    private void Remove(string id)
    {
        if (Entries.FirstOrDefault(e => e.Id == id) is { } a) Entries.Remove(a);
        if (FilteredEntries.FirstOrDefault(e => e.Id == id) is { } b) FilteredEntries.Remove(b);
        Changed(nameof(EmptyText));
        Changed(nameof(FilteredTitle));
        EntriesChanged?.Invoke(this, EventArgs.Empty);
    }
}

// One row of the Calls page: the entry and the name the person knows it by.
public sealed record CallRow(CallLogEntry Entry, string Name, string? Key)
{
    public string Detail => CallPresentation.Detail(Entry);
    public string TimeLabel => ConversationPresentation.ActivityLabel(Entry.StartedAt, DateTimeOffset.Now);
    public bool IsMissed => Entry.IsMissed;
    public bool CanCallBack => Key is not null;
    public bool CanReport => Entry.IsIncoming && Key is not null;
    public string Initials => ConversationPresentation.Initials(Name);
    public string AccessibleName => $"{Name}, {CallPresentation.OutcomeLabel(Entry).ToLowerInvariant()} {CallPresentation.KindWord(Entry)} call"
        + (Entry.StartedAt > 0 ? ", " + TimeLabel : "");
    public string CallBackLabel => $"Call {Name} back, {CallPresentation.KindWord(Entry)}";

    public static CallRow For(CallLogEntry entry, IReadOnlyList<Contact> contacts)
    {
        var key = entry.PeerKey;
        var people = entry.Participants?.Count ?? 0;
        var name = key is null ? people > 0 ? people + " people" : "Call"
            : ConversationBuilder.FindContact(key, contacts)?.Alias is { Length: > 0 } alias ? alias : PostAddress.DisplayName(key);
        return new CallRow(entry, name, key is null ? null : PostAddress.Canonical(key));
    }
}

// The Filtered list (SDS §13.4): filtered mail grouped by sender, newest first, blocked senders left out.
public sealed record FilteredSender(string Peer, string Name, IReadOnlyList<InboxMessage> Messages)
{
    public long Last => Messages.Count == 0 ? 0 : Messages.Max(m => m.At);
    public string Preview => Messages.LastOrDefault() is { } m ? RequestEnvelope.DisplayText(m.Body).ReplaceLineEndings(" ") : "";
    public string Initials => ConversationPresentation.Initials(Name);
    public string CountLabel => Messages.Count == 1 ? "1 message" : $"{Messages.Count} messages";
    public string AccessibleName => $"{Name}, {CountLabel}. {Preview}";

    public static IReadOnlyList<FilteredSender> Group(IEnumerable<InboxMessage> messages, IReadOnlyList<Contact> contacts) =>
        messages.Where(m => !m.IsOutgoing)
            .GroupBy(m => PostAddress.Canonical(m.SenderHandle ?? m.PeerHandle ?? m.From ?? m.Peer ?? ""), StringComparer.Ordinal)
            .Where(g => g.Key.Length > 1 && !BlockRules.IsBlocked(g.Key, contacts))
            .Select(g => new FilteredSender(g.Key, PostAddress.DisplayName(g.Key), g.OrderBy(m => m.At).ToArray()))
            .OrderByDescending(s => s.Last).ToArray();
}
