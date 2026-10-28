using System.Collections.ObjectModel;
using System.Globalization;

namespace Pigeonpost.Core.Meetings;

public sealed record MeetingRow(MeetingSummary Meeting)
{
    public string Id => Meeting.Id;
    public bool IsReady => Meeting.State == "ready";
    public string When => Meeting.CreatedAt is { } at and > 0 and <= 253402300799
        ? DateTimeOffset.FromUnixTimeSeconds(at).ToLocalTime().ToString("f", CultureInfo.CurrentCulture) : "Meeting";
    public string StateLabel => Meeting.State switch
    {
        "active" => "Notes in progress", "processing" => "Being prepared…", "ready" => "Ready", "failed" => "Couldn't be made", _ => "Unknown",
    };
    public string Detail => StateLabel + (Meeting.Host is { Length: > 0 } host ? " · host " + PostAddress.DisplayName(host) : "")
        + (IsReady && Meeting.ExpiresAt is { } expires and > 0 and <= 253402300799
            ? " · kept until " + DateTimeOffset.FromUnixTimeSeconds(expires).ToLocalTime().ToString("d", CultureInfo.CurrentCulture) : "");
    public string AccessibleName => $"Meeting notes, {When}, {Detail}";
}

public sealed record MeetingOpenResult(string Id, MeetingBrief? Brief, string? Message);

// The Meeting notes screen (docs/meeting-summaries.md §1.5, site-inbox/meeting-notes.js). Bound to
// one accepted login and the person it belongs to: a new login, a sign-out or another person
// retires every request, result and key reference of the previous one, so notes never show under
// a different account. Construct and use on the UI context.
public sealed class MeetingNotesModel(PostboxClient postbox, INotesKeyStore store, string label = "Windows app") : ObservableObject, IDisposable
{
    private sealed class Actor(AuthorizationLease? accepted, string? owner)
    {
        internal readonly AuthorizationLease? Accepted = accepted;
        internal readonly AuthorizationLease Authorization = accepted?.Child() ?? new AuthorizationLease();
        internal readonly string? Owner = owner;
        internal NotesDeviceKey? Key;
        internal Task<NotesDeviceKey>? Loading;
        internal bool Registered;
    }
    private Actor? actor;
    private int version, listVersion;
    private bool enabled, loading, disposed;
    private string? error;
    private CancellationTokenSource scope = new();

    private Actor? ActiveActor => !disposed && actor is { } value && value.Authorization.IsActive
        && ReferenceEquals(value.Accepted, postbox.AcceptedAuthorization) && value.Owner == postbox.OwnerKey ? value : null;
    // Results belong to the login that asked (version changes only when the login is retired).
    private bool IsCurrent(Actor value, int generation) => generation == version && ReferenceEquals(ActiveActor, value);
    private IDisposable Bind(Actor value)
    {
        if (!ReferenceEquals(ActiveActor, value)) throw new AuthorizationRetiredException();
        return AuthorizationContext.BindActor(value.Accepted, value.Authorization);
    }

    public ObservableCollection<MeetingRow> Meetings { get; } = [];
    public bool Enabled { get => enabled; private set => Set(ref enabled, value); }
    public bool IsLoading { get => loading; private set { if (Set(ref loading, value)) Changed(nameof(EmptyText)); } }
    public string? Error { get => error; private set { if (Set(ref error, value)) Changed(nameof(EmptyText)); } }
    public bool Loaded { get; private set; }
    public string EmptyText => Error ?? (!Loaded || IsLoading || Meetings.Count > 0 ? "" : "No meeting notes yet.");
    public string? DeviceKeyId => ActiveActor?.Key?.KeyId;
    public const string Explanation = "Notes from calls where you agreed to them. They're encrypted for your devices that were in the call or set up for notes; Pigeonpost's servers keep only the encrypted copy, and delete it after the retention date.";

    // After sign-in, a login change or new capabilities. The same login keeps its state; anything
    // else starts from nothing.
    public void Activate(ServerFeatures features)
    {
        if (disposed) return;
        var on = features.Has(ServerFeatures.MeetingSummaries);
        var accepted = postbox.AcceptedAuthorization;
        if (on == Enabled && ActiveActor is { } current && ReferenceEquals(current.Accepted, accepted)) return;
        Retire();
        if (!on || accepted is { IsActive: false }) return;
        try { actor = new Actor(accepted, postbox.OwnerKey); } catch (AuthorizationRetiredException) { return; }
        Enabled = true;
    }

    // Sign-out, or the account went away: nothing of it stays on screen or in flight.
    public void Reset() => Retire();

    private void Retire()
    {
        version++;
        var prior = actor;
        actor = null;
        prior?.Authorization.Retire();
        prior?.Key?.Dispose();
        scope.Cancel();
        scope.Dispose();
        scope = new();
        Enabled = false;
        Loaded = false;
        IsLoading = false;
        Error = null;
        Meetings.Clear();
        Changed(nameof(EmptyText));
    }

    // This device's key for the signed-in person: the saved one, or a new one saved for them. A
    // session that cannot name its person keeps the key for this login only.
    private async Task<NotesDeviceKey> KeyAsync(Actor owner)
    {
        var task = owner.Loading ??= LoadKeyAsync(owner);
        try { return await task; }
        catch { if (ReferenceEquals(owner.Loading, task)) owner.Loading = null; throw; }
    }
    private async Task<NotesDeviceKey> LoadKeyAsync(Actor owner)
    {
        NotesDeviceKey? key = null;
        if (NotesOwner.Valid(owner.Owner))
        {
            try { key = await store.LoadAsync(owner.Owner!); }
            catch (MeetingNotesException) { key = null; } // unreadable: replaced below
        }
        if (key is null)
        {
            key = NotesDeviceKey.Create();
            if (NotesOwner.Valid(owner.Owner)) await store.SaveAsync(owner.Owner!, key);
        }
        if (!owner.Authorization.IsActive) { key.Dispose(); throw new AuthorizationRetiredException(); }
        owner.Key = key;
        return key;
    }

    // Registers this device's notes key for the account (once per login), so notes made from now
    // on can be opened here. Idempotent on the postbox.
    public async Task RegisterAsync()
    {
        if (ActiveActor is not { Registered: false } owner) return;
        var generation = version; var token = scope.Token;
        try
        {
            using var operation = Bind(owner);
            var validate = postbox.CaptureAuthorizationValidation();
            var key = await KeyAsync(owner);
            validate();
            if (!IsCurrent(owner, generation)) return;
            try { await postbox.RegisterNotesKeyAsync(key, label, token); }
            catch (PostboxException ex) when (ex.Code == "key_exists")
            {
                // The id is bound to another key on the postbox: this device makes a new one.
                var fresh = NotesDeviceKey.Create();
                if (NotesOwner.Valid(owner.Owner)) await store.SaveAsync(owner.Owner!, fresh);
                validate();
                if (!IsCurrent(owner, generation)) { fresh.Dispose(); return; }
                owner.Key?.Dispose(); owner.Key = fresh; owner.Loading = Task.FromResult(fresh);
                await postbox.RegisterNotesKeyAsync(fresh, label, token);
            }
            validate();
            if (IsCurrent(owner, generation)) owner.Registered = true;
        }
        catch (OperationCanceledException) { }
        catch (Exception) { /* tried again on the next load */ }
    }

    public async Task LoadAsync()
    {
        if (ActiveActor is not { } owner) return;
        var generation = version; var load = ++listVersion; var token = scope.Token;
        bool Current() => IsCurrent(owner, generation) && load == listVersion;
        IsLoading = true;
        try
        {
            await RegisterAsync();
            if (!Current()) return;
            using var operation = Bind(owner);
            var validate = postbox.CaptureAuthorizationValidation();
            var list = await postbox.ListMeetingsAsync(token);
            validate();
            if (!Current()) return;
            StableList.Reconcile(Meetings, list.Take(200).Select(m => new MeetingRow(m)).ToArray(), r => r.Id, (a, b) => a == b);
            Loaded = true;
            Error = null;
        }
        catch (OperationCanceledException) { }
        catch (PostboxException ex) when (ex.Code == "feature_disabled") { if (Current()) { Meetings.Clear(); Loaded = true; Error = null; } }
        catch (Exception) { if (Current()) Error = "Couldn't load your meeting notes."; }
        finally { if (Current()) { IsLoading = false; Changed(nameof(EmptyText)); } }
    }

    // Opens one meeting's notes on this device. The answer belongs to the login that asked; a
    // stale answer is dropped (null).
    public async Task<MeetingOpenResult?> OpenAsync(string id)
    {
        if (ActiveActor is not { } owner || !PostboxClient.ValidMeetingId(id)) return null;
        var generation = version; var token = scope.Token;
        try
        {
            using var operation = Bind(owner);
            var validate = postbox.CaptureAuthorizationValidation();
            var record = await postbox.GetMeetingAsync(id, token);
            validate();
            if (!IsCurrent(owner, generation)) return null;
            if (record.State != "ready")
                return new(id, null, record.State == "failed" ? Failure(record.Failure) : "The notes are still being prepared. Check back in a few minutes.");
            var key = await KeyAsync(owner);
            validate();
            if (!IsCurrent(owner, generation)) return null;
            var brief = MeetingNotesCrypto.Open(record, key);
            return IsCurrent(owner, generation) ? new(id, brief, null) : null;
        }
        catch (OperationCanceledException) { return null; }
        catch (MeetingNotesException ex) { return IsCurrent(owner, generation) ? new(id, null, ex.Message) : null; }
        catch (PostboxException ex) when (ex.StatusCode is 404 or 410) { return IsCurrent(owner, generation) ? new(id, null, "These notes are no longer available.") : null; }
        catch (Exception) { return IsCurrent(owner, generation) ? new(id, null, "These notes couldn't be loaded. Please try again.") : null; }
    }

    // Removes this account's copy (the brief goes when the last attendee removes theirs).
    public async Task<bool> DeleteAsync(string id)
    {
        if (ActiveActor is not { } owner || !PostboxClient.ValidMeetingId(id)) return false;
        var generation = version; var token = scope.Token;
        try
        {
            using var operation = Bind(owner);
            var validate = postbox.CaptureAuthorizationValidation();
            await postbox.DeleteMeetingAsync(id, token);
            validate();
            if (!IsCurrent(owner, generation)) return false;
            if (Meetings.FirstOrDefault(m => m.Id == id) is { } row) Meetings.Remove(row);
            Changed(nameof(EmptyText));
            return true;
        }
        catch (Exception) { return false; }
    }

    private static string Failure(string? code) => code switch
    {
        "no_speech" => "No speech was captured.",
        "no_recipients" => "No device could receive the notes.",
        "stt_unavailable" => "Transcription was unavailable.",
        "timed_out" => "The notes service didn't finish in time.",
        _ => "The notes couldn't be made.",
    };

    public void Dispose()
    {
        if (disposed) return;
        Retire();
        disposed = true;
        scope.Dispose();
    }
}
