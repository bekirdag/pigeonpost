package dev.pigeonpost.core

/**
 * The quick filters above the conversation list (the web inbox's All · Unread · Agents, plus People).
 * They narrow what is shown and change nothing. An agent is a mailbox the account itself holds, or one
 * of its Nests — the only conversations the app knows to be software. People is exactly the rest.
 */
enum class ListFilter(val label: String, val empty: String) {
    ALL("All", ""),
    UNREAD("Unread", "Nothing unread."),
    AGENTS("Agents", "No conversations with your agents yet."),
    PEOPLE("People", "No conversations with people yet.");

    fun matches(conversation: Conversation, isNest: (String) -> Boolean = { false }): Boolean = when (this) {
        ALL -> true
        UNREAD -> conversation.unread > 0
        AGENTS -> isAgent(conversation, isNest)
        PEOPLE -> !isAgent(conversation, isNest)
    }

    /** Groups have no single counterpart: they stay under All, and under Unread while they have unread messages. */
    fun showsGroup(unread: Int): Boolean = this == ALL || (this == UNREAD && unread > 0)

    /** Group invitations wait for an answer, so they count as unread. */
    val showsInvitations get() = this == ALL || this == UNREAD

    companion object {
        fun isAgent(conversation: Conversation, isNest: (String) -> Boolean = { false }) = conversation.mine || isNest(conversation.peer)
        fun from(name: String?) = entries.firstOrNull { it.name == name } ?: ALL
    }
}
