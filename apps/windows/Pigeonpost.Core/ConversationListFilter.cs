namespace Pigeonpost.Core;

// The quick filters above the conversation list, matching the web inbox's All · Unread · Agents ·
// People chips. They narrow what is shown and change nothing. An agent is a mailbox the account
// itself holds or one of its Nests (the only peers the app knows to be software); People is the exact
// inverse of that, so every conversation is in exactly one of Agents and People.
public enum ConversationListFilter { All, Unread, Agents, People }

public static class ConversationListFilters
{
    public static bool IsAgent(Conversation conversation, Func<string, bool>? agentPeer) =>
        conversation.IsMine || agentPeer?.Invoke(conversation.Peer) == true;

    public static bool Matches(Conversation conversation, ConversationListFilter filter, Func<string, bool>? agentPeer) => filter switch
    {
        ConversationListFilter.Unread => conversation.Unread > 0,
        ConversationListFilter.Agents => IsAgent(conversation, agentPeer),
        ConversationListFilter.People => !IsAgent(conversation, agentPeer),
        _ => true,
    };

    public static string EmptyMessage(ConversationListFilter filter) => filter switch
    {
        ConversationListFilter.Unread => "Nothing unread.",
        ConversationListFilter.Agents => "No conversations with your agents yet.",
        ConversationListFilter.People => "No conversations with people yet.",
        _ => "No conversations yet",
    };
}
