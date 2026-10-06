using System.Collections.ObjectModel;

namespace Pigeonpost.Core;

// Preserve collection and unchanged row identities so polling does not reset native list controls.
public static class StableList
{
    public static void Reconcile<T>(ObservableCollection<T> target, IReadOnlyList<T> next,
        Func<T, string> key, Func<T, T, bool> equivalent)
    {
        var keys = next.Select(key).ToHashSet(StringComparer.Ordinal);
        for (var i = target.Count - 1; i >= 0; i--)
            if (!keys.Contains(key(target[i]))) target.RemoveAt(i);
        for (var i = 0; i < next.Count; i++)
        {
            var wanted = key(next[i]);
            if (i >= target.Count || key(target[i]) != wanted)
            {
                var existing = -1;
                for (var j = i + 1; j < target.Count; j++)
                    if (key(target[j]) == wanted) { existing = j; break; }
                if (existing < 0) target.Insert(i, next[i]);
                else target.Move(existing, i);
            }
            if (!equivalent(target[i], next[i])) target[i] = next[i];
        }
    }

    public static bool SameMessage(ThreadMessage a, ThreadMessage b) =>
        a with { Attachments = b.Attachments } == b && (a.Attachments ?? []).SequenceEqual(b.Attachments ?? []);
    public static bool SameMessages(IReadOnlyList<ThreadMessage> a, IReadOnlyList<ThreadMessage> b) =>
        a.Count == b.Count && a.Zip(b).All(pair => SameMessage(pair.First, pair.Second));
    private static bool SameContact(Contact? a, Contact? b) => a is null || b is null ? a == b :
        a with { AllowedVerbs = b.AllowedVerbs } == b && (a.AllowedVerbs ?? []).SequenceEqual(b.AllowedVerbs ?? []);
    public static bool SameConversation(Conversation a, Conversation b) =>
        a with { Messages = b.Messages, Contact = b.Contact } == b && SameContact(a.Contact, b.Contact) && SameMessages(a.Messages, b.Messages);
    public static bool SameSubject(Subject a, Subject b) =>
        a with { Messages = b.Messages } == b && SameMessages(a.Messages, b.Messages);
}
