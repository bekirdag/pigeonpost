package dev.pigeonpost.core.enterprise

import dev.pigeonpost.core.*
import kotlinx.coroutines.*
import org.junit.Assert.*
import org.junit.Test
import java.util.concurrent.Executors

class OrganizationPresenceTest {
    private fun org(id: String, status: String = "active", roles: List<String> = emptyList(), mailbox: String? = "/k/$id") =
        RoomOrganization(id, "Org $id", status, "member_$id", roles = roles, mailbox = mailbox?.let { RoomMailbox(it) })

    @Test fun roleUsesTheHighestPortalRole() {
        assertEquals("Owner", OrganizationPresence.role(listOf("admin", "owner")))
        assertEquals("Admin", OrganizationPresence.role(listOf("people_admin", "admin")))
        assertEquals("People admin", OrganizationPresence.role(listOf("people_admin")))
        assertEquals("Agent admin", OrganizationPresence.role(listOf("agent_admin")))
        assertEquals("Billing admin", OrganizationPresence.role(listOf("billing_admin")))
        assertEquals("Member", OrganizationPresence.role(emptyList()))
        assertEquals("Member", OrganizationPresence.role(listOf("something_new")))
    }

    @Test fun badgesAreKeyedByTheMembersOwnReadableOrgMailbox() {
        val catalog = RoomOrganizations(listOf(org("org_a", roles = listOf("admin")), org("org_b", status = "suspended"), org("org_c", mailbox = null),
            org("org_d", status = "read_only")))
        val badges = OrganizationPresence.badges(catalog)
        assertEquals(setOf("/k/org_a", "/k/org_d"), badges.keys)
        assertEquals(OrganizationBadge("org_a", "Org org_a", "Admin"), badges["/k/org_a"])
        assertEquals("Org org_d · Member", badges.getValue("/k/org_d").label)
        assertTrue(OrganizationPresence.badges(null).isEmpty())
        // One invalid row refuses the whole catalog, as everywhere else.
        assertTrue(OrganizationPresence.badges(RoomOrganizations(listOf(org("org a")))).isEmpty())
    }

    @Test fun directoryQueriesAreTrimmedAndCannotCarryWildcards() {
        assertEquals("acme", OrganizationPresence.query("  /acme "))
        assertEquals("acme/jo", OrganizationPresence.query("/acme/jo/"))
        assertEquals("abc", OrganizationPresence.query("a%b_c"))
        assertNull(OrganizationPresence.query(""))
        assertNull(OrganizationPresence.query("  /"))
        assertEquals(80, OrganizationPresence.query("x".repeat(100))!!.length)
    }

    @Test fun directoryEntriesAreChecked() {
        assertTrue(OrganizationDirectoryEntry("/acme/john", "/k/abc", "member", "John").valid)
        assertTrue(OrganizationDirectoryEntry("/acme/bot", "/k/abd", "agent").valid)
        assertFalse(OrganizationDirectoryEntry("/acme/john", "/k/abc", "bot").valid)
        assertFalse(OrganizationDirectoryEntry("john", "/k/abc", "member").valid)
        assertFalse(OrganizationDirectoryEntry("/acme/john", "/acme/john", "member").valid)
        assertEquals("/acme/bot", OrganizationDirectoryEntry("/acme/bot", "/k/abd", "agent").title)
        assertEquals("John", OrganizationDirectoryEntry("/acme/john", "/k/abc", "member", "John").title)
    }

    private class Api(private val source: () -> OrganizationRoomIdentity?) : OrganizationRoomApi, OrganizationDirectoryApi {
        var catalogs = 0
        val queries = mutableListOf<Pair<String, String>>()
        private suspend fun check(mailbox: String) {
            val request = currentCoroutineContext()[AuthorizationContext]?.lease ?: throw AuthorizationRetired()
            request.requireActive(); val id = source() ?: throw AuthorizationRetired()
            check(id.mailbox == mailbox && request.sharesAuthorization(id.authorization))
        }
        override suspend fun organizations(mailbox: String): RoomOrganizations {
            check(mailbox); catalogs++
            return RoomOrganizations(listOf(RoomOrganization("org_a", "Acme", "active", "m_1", roles = listOf("people_admin"), mailbox = RoomMailbox("/k/member"))))
        }
        override suspend fun directory(mailbox: String, organizationId: String, query: String): List<OrganizationDirectoryEntry> {
            check(mailbox); queries += organizationId to query
            return listOf(OrganizationDirectoryEntry("/acme/me", "/k/member", "member", "Me"), OrganizationDirectoryEntry("/acme/ayla", "/k/ayla", "member", "Ayla"))
        }
        override suspend fun rooms(mailbox: String, organizationId: String, search: String) = error("unused")
        override suspend fun preview(mailbox: String, groupId: String) = error("unused")
        override suspend fun group(mailbox: String, groupId: String) = error("unused")
        override suspend fun create(mailbox: String, payload: OrganizationRoomCreate) = error("unused")
        override suspend fun join(mailbox: String, groupId: String) = error("unused")
        override suspend fun acceptInvitation(mailbox: String, groupId: String) = error("unused")
    }

    @Test fun storeKeepsBadgesAcrossInboxesSearchesOnlyAsAnOrgMailboxAndForgetsOnSignOut() = runBlocking<Unit> {
        val dispatcher = Executors.newSingleThreadExecutor { Thread(it, "presence-test").apply { isDaemon = true } }.asCoroutineDispatcher()
        val scope = CoroutineScope(SupervisorJob() + dispatcher)
        val lease = AuthorizationLease()
        var source: OrganizationRoomIdentity? = OrganizationRoomIdentity("person", lease, "/k/member")
        val api = Api { source }
        val store = OrganizationPresenceStore(api, api, scope, { source }, debounceMillis = 0)
        try {
            withTimeout(8_000) { withContext(dispatcher) {
                store.synchronize(); store.refresh()?.join()
                assertEquals("Acme · People admin", store.state.value.badges.getValue("/k/member").label)
                assertEquals(1, api.catalogs)

                store.search("ay")!!.join()
                assertEquals(listOf("org_a" to "ay"), api.queries)
                assertEquals(listOf("/acme/ayla"), store.state.value.directory.map { it.handle }) // never suggests yourself

                // Another inbox of the same person: badges stay, no directory for a personal mailbox.
                source = OrganizationRoomIdentity("person", lease, "/k/personal"); store.synchronize()
                assertTrue(store.state.value.directory.isEmpty())
                assertTrue("/k/member" in store.state.value.badges)
                assertNull(store.search("ay"))
                assertEquals(1, api.queries.size)
                assertEquals(1, api.catalogs)

                // Sign out: everything goes.
                lease.retire(); source = null; store.synchronize()
                assertTrue(store.state.value.badges.isEmpty())
            } }
        } finally { scope.cancel(); dispatcher.close() }
    }
}
