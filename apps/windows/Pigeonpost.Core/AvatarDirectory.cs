namespace Pigeonpost.Core;

// Profile pictures for the addresses on screen (SDS §16). Lookups are batched and cached by
// address; image bytes are cached by content hash, so a picture shared by several addresses (or
// unchanged across a refresh) downloads once. "No picture" and "not visible to you" look the same.
// Construct and use on the UI context.
public sealed class AvatarDirectory(PostboxClient postbox, Func<DateTimeOffset>? clock = null)
{
    private static readonly TimeSpan Fresh = TimeSpan.FromMinutes(10);
    private const int MaxImages = 300;
    private readonly Dictionary<string, (AvatarInfo? Info, DateTimeOffset At)> known = new(StringComparer.Ordinal);
    private readonly Dictionary<string, byte[]> images = new(StringComparer.Ordinal);
    private readonly LinkedList<string> order = new();
    private readonly Dictionary<string, Task<byte[]?>> downloads = new(StringComparer.Ordinal);
    private readonly HashSet<string> asking = new(StringComparer.Ordinal);
    private DateTimeOffset Now => clock?.Invoke() ?? DateTimeOffset.UtcNow;

    public bool Enabled { get; set; }
    // Raised with the address whose picture changed (appeared, changed or went away).
    public event EventHandler<string>? PictureChanged;

    public static string Key(string address) => address.StartsWith("/k/", StringComparison.Ordinal) ? address : PostAddress.Canonical(address);

    public AvatarInfo? Known(string address) => known.TryGetValue(Key(address), out var row) ? row.Info : null;

    public async Task RefreshAsync(IEnumerable<string> addresses, CancellationToken cancellationToken, bool force = false)
    {
        if (!Enabled) return;
        var now = Now;
        var wanted = addresses.Where(a => !string.IsNullOrWhiteSpace(a) && a.StartsWith('/') && !a.EndsWith("/*", StringComparison.Ordinal))
            .Select(Key).Distinct(StringComparer.Ordinal)
            .Where(k => !asking.Contains(k) && (force || !known.TryGetValue(k, out var row) || now - row.At > Fresh)).ToArray();
        if (wanted.Length == 0) return;
        foreach (var key in wanted) asking.Add(key);
        try
        {
            var found = await postbox.LookupAvatarsAsync(wanted, cancellationToken);
            foreach (var key in wanted)
            {
                var info = found.GetValueOrDefault(key);
                var before = known.TryGetValue(key, out var old) ? old.Info : null;
                known[key] = (info, Now);
                if (before?.Hash != info?.Hash || before?.DisplayUrl != info?.DisplayUrl) PictureChanged?.Invoke(this, key);
            }
        }
        catch (Exception ex) when (ex is not OperationCanceledException) { /* Initials stay; the next refresh retries. */ }
        finally { foreach (var key in wanted) asking.Remove(key); }
    }

    // The picture's JPEG bytes, or null for initials.
    public Task<byte[]?> ImageAsync(string address, CancellationToken cancellationToken)
    {
        if (!Enabled || Known(address) is not { DisplayUrl: { } url } info) return Task.FromResult<byte[]?>(null);
        var cacheKey = info.Hash + "|" + url;
        if (images.TryGetValue(cacheKey, out var bytes)) { Touch(cacheKey); return Task.FromResult<byte[]?>(bytes); }
        if (downloads.TryGetValue(cacheKey, out var running)) return running;
        var task = DownloadAsync(cacheKey, url, cancellationToken);
        downloads[cacheKey] = task;
        return task;
    }

    private async Task<byte[]?> DownloadAsync(string cacheKey, string url, CancellationToken cancellationToken)
    {
        try
        {
            var bytes = await postbox.GetAvatarImageAsync(url, cancellationToken);
            images[cacheKey] = bytes;
            Touch(cacheKey);
            while (order.Count > MaxImages && order.Last is { } oldest) { images.Remove(oldest.Value); order.RemoveLast(); }
            return bytes;
        }
        catch (Exception ex) when (ex is not OperationCanceledException) { return null; }
        finally { downloads.Remove(cacheKey); }
    }

    private void Touch(string cacheKey)
    {
        order.Remove(cacheKey);
        order.AddFirst(cacheKey);
    }

    // A `profile.avatar` event or the person's own change: ask again next time.
    public void Forget(IEnumerable<string> addresses)
    {
        foreach (var key in addresses.Select(Key))
            if (known.Remove(key)) PictureChanged?.Invoke(this, key);
    }

    public void Clear()
    {
        known.Clear();
        images.Clear();
        order.Clear();
    }
}
