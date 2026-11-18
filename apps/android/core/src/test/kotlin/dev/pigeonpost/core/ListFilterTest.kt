package dev.pigeonpost.core

import org.junit.Assert.*
import org.junit.Test

class ListFilterTest {

    @Test
    fun testAllMatchesEverything() {
        val conv = Conversation("peer", listOf(), identity = null)
        assertTrue(ListFilter.ALL.matches(conv))
    }

    @Test
    fun testUnreadOnlyMatchesConversationsWithUnreadMessages() {
        val unreadMsg = ThreadMessage(id = "1", body = "hi", outgoing = false, at = 0L, read = false)
        val convWithUnread = Conversation("peer", listOf(unreadMsg), identity = null)
        val convWithoutUnread = Conversation("peer", listOf(), identity = null)

        assertTrue(ListFilter.UNREAD.matches(convWithUnread))
        assertFalse(ListFilter.UNREAD.matches(convWithoutUnread))
    }

    @Test
    fun testAgentsMatchesOwnMailboxOrNestPeer() {
        val ownMailboxConv = Conversation("peer", listOf(), identity = Mailbox(address = "me@example.com"))
        val nestPeerConv = Conversation("nest-peer", listOf(), identity = null)
        val regularPeerConv = Conversation("regular-peer", listOf(), identity = null)

        val isNest: (String) -> Boolean = { it == "nest-peer" }

        assertTrue(ListFilter.AGENTS.matches(ownMailboxConv, isNest))
        assertTrue(ListFilter.AGENTS.matches(nestPeerConv, isNest))
        assertFalse(ListFilter.AGENTS.matches(regularPeerConv, isNest))
    }

    @Test
    fun testPeopleIsInverseOfAgents() {
        val ownMailboxConv = Conversation("peer", listOf(), identity = Mailbox(address = "me@example.com"))
        val nestPeerConv = Conversation("nest-peer", listOf(), identity = null)
        val regularPeerConv = Conversation("regular-peer", listOf(), identity = null)

        val isNest: (String) -> Boolean = { it == "nest-peer" }

        assertFalse(ListFilter.PEOPLE.matches(ownMailboxConv, isNest))
        assertFalse(ListFilter.PEOPLE.matches(nestPeerConv, isNest))
        assertTrue(ListFilter.PEOPLE.matches(regularPeerConv, isNest))
    }

    @Test
    fun testShowsGroupAndInvitations() {
        assertTrue(ListFilter.ALL.showsGroup(0))
        assertTrue(ListFilter.UNREAD.showsGroup(1))
        assertFalse(ListFilter.UNREAD.showsGroup(0))
        assertFalse(ListFilter.AGENTS.showsGroup(1))

        assertTrue(ListFilter.ALL.showsInvitations)
        assertTrue(ListFilter.UNREAD.showsInvitations)
        assertFalse(ListFilter.AGENTS.showsInvitations)
        assertFalse(ListFilter.PEOPLE.showsInvitations)
    }

    @Test
    fun testFromFallsBackToAllForNullOrUnknownNames() {
        assertEquals(ListFilter.ALL, ListFilter.from(null))
        assertEquals(ListFilter.ALL, ListFilter.from("UNKNOWN"))
        assertEquals(ListFilter.UNREAD, ListFilter.from("UNREAD"))
    }
}
