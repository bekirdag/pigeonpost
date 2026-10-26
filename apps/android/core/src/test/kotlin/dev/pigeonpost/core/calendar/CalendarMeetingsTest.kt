package dev.pigeonpost.core.calendar

import dev.pigeonpost.core.AuthorizationLease
import dev.pigeonpost.core.AuthorizationRetired
import dev.pigeonpost.core.WireJson
import org.junit.Assert.*
import org.junit.Test

class CalendarMeetingsTest {
    private fun retired(work: () -> Unit) { try { work(); fail("Expected a retired join") } catch (_: AuthorizationRetired) { } }
    @Test fun meetingCodesNormalizeOnlyTheTwoExplicitAsciiForms() {
        for (valid in listOf("ABCDEFGHJK", "abcdefghjk", "ABCDE-FGHJK")) assertEquals("ABCDEFGHJK", CalendarMeetingCode.parse(valid))
        for (invalid in listOf("ABCDEF", "ABCDEFGHJI", "ABCDEFGHJO", "ABCDEFGHJL", "ABCDEFGHJ1", "ABCDEFGHJ0", " ABCDEFGHJK", "ABCDEFGHJK ", "-ABCDE-FGHJK", "ABCD-EFGH-JK", "ABCDE/FGHJK", "ABCDE%46GHJK", "ＡBCDEFGHJK")) assertNull(CalendarMeetingCode.parse(invalid))
    }
    @Test fun publicLinksContainOnlyAMeetingLocator() {
        assertEquals(CalendarLink.Member("ABCDEFGHJK"), CalendarLink.parse("https://inbox.pigeonpost.dev/m/ABCDE-FGHJK"))
        assertEquals(CalendarLink.Member("ABCDEFGHJK"), CalendarLink.parse("dev.pigeonpost.inbox://m/abcdefghjk"))
        assertNull(CalendarLink.parse("https://inbox.pigeonpost.dev/m/ABCDEF"))
        assertNull(CalendarLink.parse("https://inbox.pigeonpost.dev/m/evt_" + "a".repeat(26)))
    }
    @Test fun guestInvitationsRemainBrowserLinksAndNeverBecomeMemberAuthority() {
        for (url in listOf("https://inbox.pigeonpost.dev/m/ABCDEFGHJK#t=private-token", "https://inbox.pigeonpost.dev/m/evt_" + "a".repeat(26) + "#t=private-token&r=accepted", "https://inbox.pigeonpost.dev/m/u#u=private-token")) {
            val link = CalendarLink.parse(url)
            assertTrue(link is CalendarLink.Guest); assertEquals(url, (link as CalendarLink.Guest).url)
            assertFalse(link.toString().contains("private-token"))
        }
        val custom = CalendarLink.parse("dev.pigeonpost.inbox://m/ABCDEFGHJK#t=private-token") as CalendarLink.Guest
        assertEquals("https://inbox.pigeonpost.dev/m/ABCDEFGHJK#t=private-token", custom.url)
    }
    @Test fun portsCredentialsQueriesAlternateOriginsAndEncodedPathsAreRefused() {
        for (url in listOf("http://inbox.pigeonpost.dev/m/ABCDEFGHJK", "https://inbox.pigeonpost.dev.evil.invalid/m/ABCDEFGHJK", "https://user@inbox.pigeonpost.dev/m/ABCDEFGHJK", "https://inbox.pigeonpost.dev:443/m/ABCDEFGHJK", "https://inbox.pigeonpost.dev/m/ABCDEFGHJK?role=host", "https://inbox.pigeonpost.dev/m/ABCDEFGHJK?", "https://inbox.pigeonpost.dev/m/ABCDE%2FFGHJK", "https://inbox.pigeonpost.dev/m/../m/ABCDEFGHJK", "dev.pigeonpost.inbox://oauth2redirect/m/ABCDEFGHJK")) assertNull(CalendarLink.parse(url))
    }
    @Test fun fragmentKeysAndCapabilitiesHaveStrictBoundsAndNoDuplicates() {
        val prefix = "https://inbox.pigeonpost.dev/m/ABCDEFGHJK#"
        for (fragment in listOf("t=one&t=two", "t=", "token=one", "t=one&role=host", "t=one&r=host", "t=a%2Fb", "t=a+b", "t=one&r=accepted&extra=1", "u=one", "t=" + "a".repeat(2049))) assertNull(CalendarLink.parse(prefix + fragment))
        assertNull(CalendarLink.parse("https://inbox.pigeonpost.dev/m/u#u=one&t=two"))
        assertNull(CalendarLink.parse("https://inbox.pigeonpost.dev/m/ABCDEFGHJK#t=one%"))
    }
    private fun occurrence(id: String = "occ_one") = CalendarMeetingOccurrence(id, 100, 200, 50, 250)
    @Test fun onlyCompleteOpenOccurrenceWindowsBecomeSelectable() {
        assertEquals(listOf(occurrence()), CalendarMeetingStatus("open", listOf(occurrence())).choices)
        for (row in listOf(occurrence().copy(opensAt = null), occurrence().copy(startsAt = 200), occurrence().copy(opensAt = 101), occurrence().copy(closesAt = 199), occurrence().copy(opensAt = -1), occurrence().copy(closesAt = Long.MAX_VALUE), occurrence("occ_bad/path")))
            assertTrue(CalendarMeetingStatus("open", listOf(row)).choices.isEmpty())
        for (state in listOf(null, "not_open", "unavailable", "future_status")) assertTrue(CalendarMeetingStatus(state, listOf(occurrence())).choices.isEmpty())
    }
    @Test fun duplicateOrOverlargeChoiceListsGrantNoImplicitSelection() {
        assertTrue(CalendarMeetingStatus("open", listOf(occurrence(), occurrence())).choices.isEmpty())
        assertTrue(CalendarMeetingStatus("open", (1..51).map { occurrence("occ_$it") }).choices.isEmpty())
        assertEquals(50, CalendarMeetingStatus("open", (1..50).map { occurrence("occ_$it") }).choices.size)
    }
    @Test fun statusRoleClaimsAreNotAdoptedAndMissingWindowsGrantNothing() {
        val response = WireJson.decodeFromString<CalendarMeetingStatus>("""{"state":"open","role":"host","admission":"admit","occurrences":[{"occurrence_id":"occ_one"}]}""")
        assertTrue(response.choices.isEmpty())
    }
    @Test fun aReviewedJoinIsBoundToItsInboxAndAcceptedLogin() {
        val root = AuthorizationLease()
        val join = CalendarScheduledJoin("/k/alice", "ABCDE-FGHJK", "occ_original", "Private meeting title", false, root.child())
        join.requireCurrent(root, "/k/alice")
        assertEquals("ABCDEFGHJK", join.code); assertEquals("occ_original", join.occurrenceId)
        retired { join.requireCurrent(root, "/k/bob") }
        retired { join.requireCurrent(AuthorizationLease(), "/k/alice") }
        assertFalse(join.toString().contains("Private"))
    }
    @Test fun retiringTheCalendarActorRefusesDelayedPermissionResultsWithoutRevokingTheLogin() {
        val root = AuthorizationLease(); val actor = root.child()
        val join = CalendarScheduledJoin("/k/alice", "ABCDEFGHJK", "occ_original", "Title", true, actor)
        actor.retire(); retired { join.requireCurrent(root, "/k/alice") }; assertTrue(root.isValid)
    }
    @Test fun closingAJoinDoesNotRetireItsParentOrAnotherJoin() {
        val root = AuthorizationLease()
        val first = CalendarScheduledJoin("/k/alice", "ABCDEFGHJK", "occ_original", "Title", true, root)
        val next = CalendarScheduledJoin("/k/alice", "ABCDEFGHJK", "occ_original", "Title", true, root)
        first.close(); retired { first.requireCurrent(root, "/k/alice") }; next.requireCurrent(root, "/k/alice")
        assertTrue(root.isValid)
    }
}
