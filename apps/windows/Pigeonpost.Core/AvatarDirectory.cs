namespace Pigeonpost.Core;

// Profile pictures for the addresses on screen (SDS §16). Each cache belongs to one accepted
// login and one local revision; clearing it also retires every pending lookup/download.
// Construct and use on the UI context.
public sealed class AvatarDirectory(PostboxClient postbox, Func<DateTimeOffset>? clock = null)
{
    private static readonly TimeSpan Fresh = TimeSpan.FromMinutes(10);
    private const int MaxImages = 300;
    private sealed class Cache(AuthorizationLease? accepted)
    {
        internal readonly AuthorizationLease? Accepted = accepted;
        internal readonly AuthorizationLease Authorization = accepted?.Child() ?? new AuthorizationLease();
        internal readonly Dictionary<string, (AvatarInfo? Info, DateTimeOffset At)> Known = new(StringComparer.Ordinal);
        internal readonly Dictionary<string, byte[]> Images = new(StringComparer.Ordinal);
        internal readonly LinkedList<string> Order = new();
        internal readonly Dictionary<string, Task<byte[]?>> Downloads = new(StringComparer.Ordinal);
        internal readonly HashSet<string> Asking = new(StringComparer.Ordinal);
    }
    private Cache? cache;
    private long version;
    private bool enabled;
    private DateTimeOffset Now => clock?.Invoke() ?? DateTimeOffset.UtcNow;

    public bool Enabled
    {
        get => enabled;
        set { if (enabled == value) return; enabled = value; Clear(); }
    }
    public long Version { get { Current(); return version; } }
    public bool IsCurrent(long revision)
    {
        if (Current() is not { } current || revision != version || !IsCurrent(current)) return false;
        try
        {
            using var actor = AuthorizationContext.BindActor(current.Accepted, current.Authorization);
            postbox.CaptureAuthorizationValidation()();
            return true;
        }
        catch (AuthorizationRetiredException) { return false; }
    }
    // Raised with the address whose picture changed (appeared, changed or went away).
    public event EventHandler<string>? PictureChanged;

    public static string Key(string address) => address.StartsWith("/k/", StringComparison.Ordinal) ? address : PostAddress.Canonical(address);

    private Cache? Current()
    {
        if (!Enabled) return null;
        var accepted = postbox.AcceptedAuthorization;
        if (cache is not null && (!ReferenceEquals(accepted, cache.Accepted) || !cache.Authorization.IsActive)) Clear();
        if (accepted is { IsActive: false }) return null;
        return cache ??= new Cache(accepted);
    }
    private bool IsCurrent(Cache current) => Enabled && ReferenceEquals(cache, current) && current.Authorization.IsActive
        && ReferenceEquals(current.Accepted, postbox.AcceptedAuthorization);

    public AvatarInfo? Known(string address)
    {
        if (Current() is not { } current) return null;
        try
        {
            using var actor = AuthorizationContext.BindActor(current.Accepted, current.Authorization);
            postbox.CaptureAuthorizationValidation()();
            return current.Authorization.WithActive(() => current.Known.TryGetValue(Key(address), out var row) ? row.Info : null);
        }
        catch (AuthorizationRetiredException) { return null; }
    }

    public async Task RefreshAsync(IEnumerable<string> addresses, CancellationToken cancellationToken, bool force = false)
    {
        if (Current() is not { } current) return;
        var now = Now;
        var wanted = addresses.Where(a => !string.IsNullOrWhiteSpace(a) && a.StartsWith('/') && !a.EndsWith("/*", StringComparison.Ordinal))
            .Select(Key).Distinct(StringComparer.Ordinal)
            .Where(k => !current.Asking.Contains(k) && (force || !current.Known.TryGetValue(k, out var row) || now - row.At > Fresh)).ToArray();
        if (wanted.Length == 0) return;
        foreach (var key in wanted) current.Asking.Add(key);
        try
        {
            using var actor = AuthorizationContext.BindActor(current.Accepted, current.Authorization);
            var validate = postbox.CaptureAuthorizationValidation();
            var found = await postbox.LookupAvatarsAsync(wanted, cancellationToken);
            validate();
            if (!IsCurrent(current)) return;
            foreach (var key in wanted)
            {
                var info = found.GetValueOrDefault(key);
                var before = current.Known.TryGetValue(key, out var old) ? old.Info : null;
                var at = Now;
                current.Authorization.WithActive(() => { current.Known[key] = (info, at); return 0; });
                if (IsCurrent(current) && (before?.Hash != info?.Hash || before?.DisplayUrl != info?.DisplayUrl)) PictureChanged?.Invoke(this, key);
            }
        }
        catch (AuthorizationRetiredException) { }
        catch (Exception ex) when (ex is not OperationCanceledException) { /* Initials stay; the next refresh retries. */ }
        finally { foreach (var key in wanted) current.Asking.Remove(key); }
    }

    // The picture's JPEG bytes, or null for initials. Register a download before starting it:
    // even a synchronously completed stand-in must not leave a completed task in the registry.
    public Task<byte[]?> ImageAsync(string address, CancellationToken cancellationToken)
    {
        if (Current() is not { } current || Known(address) is not { DisplayUrl: { } url } info) return Task.FromResult<byte[]?>(null);
        if (!IsCurrent(current)) return Task.FromResult<byte[]?>(null);
        var cacheKey = info.Hash + "|" + url;
        if (current.Images.TryGetValue(cacheKey, out var bytes)) { Touch(current, cacheKey); return Task.FromResult<byte[]?>(bytes); }
        if (current.Downloads.TryGetValue(cacheKey, out var running)) return running;
        var completion = new TaskCompletionSource<byte[]?>(TaskCreationOptions.RunContinuationsAsynchronously);
        current.Downloads[cacheKey] = completion.Task;
        _ = DownloadAsync(current, cacheKey, url, cancellationToken, completion);
        return completion.Task;
    }

    private async Task DownloadAsync(Cache current, string cacheKey, string url, CancellationToken cancellationToken, TaskCompletionSource<byte[]?> completion)
    {
        try
        {
            using var actor = AuthorizationContext.BindActor(current.Accepted, current.Authorization);
            var validate = postbox.CaptureAuthorizationValidation();
            var bytes = await postbox.GetAvatarImageAsync(url, cancellationToken);
            validate();
            if (!IsCurrent(current)) { completion.TrySetResult(null); return; }
            current.Authorization.WithActive(() =>
            {
                current.Images[cacheKey] = bytes;
                Touch(current, cacheKey);
                while (current.Order.Count > MaxImages && current.Order.Last is { } oldest) { current.Images.Remove(oldest.Value); current.Order.RemoveLast(); }
                completion.TrySetResult(bytes);
                return 0;
            });
        }
        catch (AuthorizationRetiredException) { completion.TrySetResult(null); }
        catch (OperationCanceledException ex) { completion.TrySetCanceled(ex.CancellationToken); }
        catch (Exception) { completion.TrySetResult(null); }
        finally
        {
            if (current.Downloads.TryGetValue(cacheKey, out var pending) && ReferenceEquals(pending, completion.Task)) current.Downloads.Remove(cacheKey);
        }
    }

    private static void Touch(Cache current, string cacheKey)
    {
        current.Order.Remove(cacheKey);
        current.Order.AddFirst(cacheKey);
    }

    // Use only for short synchronous local cache writes; never await or call UI/session code.
    public bool TryWithCurrent(long revision, Action write)
    {
        if (Current() is not { } current || revision != version) return false;
        try
        {
            using var actor = AuthorizationContext.BindActor(current.Accepted, current.Authorization);
            postbox.CaptureAuthorizationValidation()();
            current.Authorization.WithActive(() => { write(); return 0; });
            return true;
        }
        catch (AuthorizationRetiredException) { return false; }
    }

    // A profile event invalidates pending work too. Keep unrelated, already-authorized metadata
    // and image bytes; repainting those addresses reuses their cache without a visible flicker.
    public void Forget(IEnumerable<string> addresses)
    {
        var forgotten = addresses.Select(Key).ToHashSet(StringComparer.Ordinal);
        var prior = Current();
        var changed = forgotten.Concat(prior?.Known.Keys.AsEnumerable() ?? []).Distinct(StringComparer.Ordinal).ToArray();
        Clear();
        if (Current() is not { } next) return;
        if (prior is not null && ReferenceEquals(prior.Accepted, next.Accepted))
        {
            foreach (var row in prior.Known.Where(row => !forgotten.Contains(row.Key))) next.Known[row.Key] = row.Value;
            var retained = next.Known.Values.Where(row => row.Info?.DisplayUrl is not null)
                .Select(row => row.Info!.Hash + "|" + row.Info.DisplayUrl).ToHashSet(StringComparer.Ordinal);
            foreach (var key in prior.Order.Reverse().Where(retained.Contains))
                if (prior.Images.TryGetValue(key, out var bytes)) { next.Images[key] = bytes; Touch(next, key); }
        }
        foreach (var key in changed) if (IsCurrent(next)) PictureChanged?.Invoke(this, key);
    }

    public void Clear()
    {
        var prior = cache;
        cache = null;
        version++;
        prior?.Authorization.Retire();
    }
}
