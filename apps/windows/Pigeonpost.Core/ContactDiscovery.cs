namespace Pigeonpost.Core;

public sealed record DiscoveredPerson(string Name, string Address, string? Handle, string? DisplayName);
public sealed record DiscoveryOutcome(IReadOnlyList<DiscoveredPerson> Found, IReadOnlyList<ImportedNumber> NoMatch,
    int Unchecked, int? RemainingToday, string? Stopped);

// Runs "Find people I know" over an imported file (docs/contacts-and-phone.md §3 and the 29 October corrections):
// batches of at most 200 hashes, stops at the returned daily allowance and keeps partial results. A number that was
// never checked is counted as unchecked, never listed as "No match found".
public static class ContactDiscovery
{
    public const int BatchSize = 200;

    public static async Task<DiscoveryOutcome> RunAsync(IReadOnlyList<ImportedNumber> numbers,
        Func<IReadOnlyList<string>, CancellationToken, Task<DiscoveryResult>> discover, CancellationToken ct)
    {
        var found = new List<DiscoveredPerson>(); var noMatch = new List<ImportedNumber>();
        var checkedCount = 0; int? remaining = null; string? stopped = null;
        while (checkedCount < numbers.Count)
        {
            ct.ThrowIfCancellationRequested();
            var size = Math.Min(BatchSize, numbers.Count - checkedCount);
            if (remaining is { } left)
            {
                if (left <= 0) { stopped = "daily"; break; }
                size = Math.Min(size, left);
            }
            var batch = numbers.Skip(checkedCount).Take(size).ToArray();
            DiscoveryResult result;
            try { result = await discover(batch.Select(n => n.Hash).ToArray(), ct).ConfigureAwait(false); }
            catch (OperationCanceledException) { throw; }
            catch (Exception error) when (found.Count > 0 || noMatch.Count > 0)
            {
                stopped = PostboxClient.PeopleRefusal(error);
                break;
            }
            var byHash = result.Matches.ToDictionary(m => m.Hash, StringComparer.Ordinal);
            foreach (var number in batch)
            {
                if (byHash.TryGetValue(number.Hash, out var match)) found.Add(new(number.Name, match.Address, match.Handle, match.DisplayName));
                else noMatch.Add(number);
            }
            checkedCount += batch.Length;
            remaining = result.Remaining;
        }
        if (stopped is null && checkedCount < numbers.Count) stopped = "daily";
        return new(found, noMatch, numbers.Count - checkedCount, remaining, stopped);
    }
}
