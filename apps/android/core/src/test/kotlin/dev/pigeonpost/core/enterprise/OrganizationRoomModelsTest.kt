package dev.pigeonpost.core.enterprise

import org.junit.Assert.*
import org.junit.Test

class OrganizationRoomModelsTest {
    private fun org() = RoomOrganization("org_fixture", "Example", "active", "member_fixture", "example",
        RoomOrganizationSettings("admins"), listOf("owner"), RoomMailbox("/k/member", "/example/alice"))
    private fun payload(name: String = "Review", description: String = "Notes") = OrganizationRoomCreate("org_fixture", name, description,
        RoomJoinPolicy.MEMBERS, RoomPostPolicy.EVERYONE, true, "rk_fixture")

    @Test fun canonicalMembershipAndExplicitReadDelegationAssociateAnInbox() {
        val catalog = RoomOrganizations(listOf(org()), listOf(
            RoomOperatedMailbox("/k/agent", "org_fixture", "/example/build", listOf("read_inbox")),
            RoomOperatedMailbox("/k/configurer", "org_fixture", "/example/configurer", listOf("configure_rules"))))
        assertEquals(listOf(org()), catalog.forMailbox("/k/member")); assertEquals(listOf(org()), catalog.forMailbox("/k/agent"))
        for (value in listOf("/example/alice", "/k/configurer", "/k/stranger", "/example/build")) assertTrue(catalog.forMailbox(value).isEmpty())
        assertFalse(org().mayCreate("/k/agent"))
    }
    @Test fun unknownRolesPoliciesAndInactiveOrganizationsCannotOfferCreation() {
        for (role in listOf("owner", "admin", "people_admin", "agent_admin", "billing_admin")) assertTrue(org().copy(roles = listOf(role)).mayCreate("/k/member"))
        for (roles in listOf(emptyList(), listOf("future_admin"))) assertFalse(org().copy(roles = roles).mayCreate("/k/member"))
        assertTrue(org().copy(roles = emptyList(), settings = RoomOrganizationSettings("everyone")).mayCreate("/k/member"))
        assertFalse(org().copy(settings = RoomOrganizationSettings("future")).mayCreate("/k/member"))
        assertTrue(org().copy(status = "read_only").readable); assertFalse(org().copy(status = "read_only").mayCreate("/k/member"))
        for (status in listOf("suspended", "future", "pending")) assertTrue(RoomOrganizations(listOf(org().copy(status = status))).forMailbox("/k/member").isEmpty())
    }
    @Test fun duplicateOrMalformedCatalogMetadataCannotAssociateAnInbox() {
        assertFalse(RoomOrganizations(listOf(org(), org())).valid)
        for (bad in listOf(org().copy(id = "../escape"), org().copy(memberId = ""), org().copy(mailbox = RoomMailbox("/example/alice")),
            org().copy(roles = listOf("owner", "owner")), org().copy(name = "Example\u202eadmin"))) {
            val catalog = RoomOrganizations(listOf(bad)); assertFalse(catalog.valid); assertTrue(catalog.forMailbox("/k/member").isEmpty())
        }
        assertFalse(RoomOrganizations(listOf(org()), listOf(RoomOperatedMailbox("/k/agent", "org_other", capabilities = listOf("read_inbox")))).valid)
    }
    @Test fun missingOrFutureMembershipNeverOpensARoom() {
        val room = OrganizationRoom("room_fixture", "Review", memberCount = 2, joinPolicy = "private")
        assertEquals(RoomAction.UNAVAILABLE, room.action)
        for (state in listOf("active", "keying")) assertEquals(RoomAction.OPEN, room.copy(membership = state).action)
        assertEquals(RoomAction.ACCEPT_INVITATION, room.copy(membership = "invited").action)
        for (state in listOf(null, "left", "removed")) assertEquals(RoomAction.JOIN, room.copy(joinPolicy = "public", membership = state).action)
        assertEquals(RoomAction.UNAVAILABLE, room.copy(joinPolicy = "public", membership = "future").action)
    }
    @Test fun creationRetainsItsEncodedIntentAndUsesUnicodeScalarLimits() {
        val input = payload("  Tasarım 🕊️  ", "Notes\nNext line")
        assertEquals("Tasarım 🕊️", input.name); assertEquals("rk_fixture", input.body.getValue("request_key").toString().trim('"'))
        assertTrue(input.encoded.contains("\"org_id\":\"org_fixture\"")); assertTrue(input.encoded.contains("\"history_visible_to_new_members\":false"))
        assertEquals(input.encoded, input.body.toString()); assertFalse(input.toString().contains("Tasarım"))
        assertEquals(80, payload("🕊".repeat(80)).name.codePointCount(0, 160))
        for (name in listOf("", " ", "🕊".repeat(81), "Review\nadmin", "Review\u202eadmin", "Review\uD800")) assertTrue(runCatching { payload(name) }.isFailure)
        assertTrue(runCatching { payload(description = "x".repeat(501)) }.isFailure)
        assertTrue(runCatching { OrganizationRoomCreate("", "Review", "", RoomJoinPolicy.MEMBERS, RoomPostPolicy.ADMINS, false, "rk_fixture") }.isFailure)
    }
}
