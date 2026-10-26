package dev.pigeonpost.core.calendar

import dev.pigeonpost.core.*
import kotlinx.coroutines.*
import org.junit.Assert.*
import org.junit.Test
import java.io.IOException
import java.util.concurrent.Executors

class CalendarMeetingLookupTest {
    private data class Call(val mailbox: String, val code: String, val lease: AuthorizationLease?)
    private class Api : CalendarMeetingApi {
        val calls = mutableListOf<Call>()
        var handler: suspend (Call) -> CalendarMeetingStatus = { open() }
        override suspend fun scheduledMeeting(mailbox: String, code: String): CalendarMeetingStatus {
            val call = Call(mailbox, code, currentCoroutineContext()[AuthorizationContext]?.lease)
            calls += call; return handler(call)
        }
    }
    private class Fixture(val base: CoroutineScope) {
        val root = AuthorizationLease()
        val scope = AuthorizedStoreScope(base).apply { activate(root) }
        val api = Api()
        var expired = 0
        val lookup = CalendarMeetingLookup(api, scope) { expired++ }
        val context get() = lookup.state.value.contextId
        fun bind(mailbox: String = "/k/alice") = lookup.onMailbox(mailbox)
        suspend fun load(code: String = "ABCDEFGHJK") { lookup.load(code, context)!!.join() }
    }
    private fun world(bind: Boolean = true, work: suspend Fixture.() -> Unit) = runBlocking<Unit> {
        val owner = Executors.newSingleThreadExecutor { Thread(it, "meeting-lookup-test").apply { isDaemon = true } }.asCoroutineDispatcher()
        val base = CoroutineScope(SupervisorJob() + owner)
        val f = Fixture(base)
        try { withTimeout(8_000) { withContext(owner) { if (bind) f.bind(); f.work() } } }
        finally { withContext(owner) { f.lookup.reset(); f.scope.retire() }; base.cancel(); owner.close() }
    }
    @Test fun anInactiveActorCannotBindOrReadAMeeting() = world(bind = false) {
        scope.retire(); bind()
        assertNull(lookup.state.value.mailbox); assertNull(lookup.load("ABCDEFGHJK", context)); assertTrue(api.calls.isEmpty())
    }
    @Test fun aReadUsesTheExactCanonicalMailboxAndNeverAutomaticallyPreparesAJoin() = world {
        load("ABCDE-FGHJK")
        assertEquals("/k/alice", api.calls.single().mailbox); assertEquals("ABCDEFGHJK", api.calls.single().code)
        assertTrue(api.calls.single().lease!!.sharesAuthorization(root)); assertFalse(api.calls.single().lease!!.isValid)
        assertEquals(1, lookup.state.value.information!!.choices.size); assertFalse(lookup.state.value.loading)
    }
    @Test fun onlyASelectedCurrentCodeAndContextCanPrepareAJoin() = world {
        load(); var prepared = 0
        assertFalse(lookup.prepareJoin("occ_one", false, "old-context", "ABCDEFGHJK") { prepared++ })
        assertFalse(lookup.prepareJoin("occ_one", false, context, "BCDEFGHJKM") { prepared++ })
        assertFalse(lookup.prepareJoin("occ_other", false, context, "ABCDEFGHJK") { prepared++ })
        assertEquals(0, prepared)
        assertTrue(lookup.prepareJoin("occ_one", false, context, "ABCDEFGHJK") { request -> prepared++; request.requireCurrent(root, "/k/alice") })
        assertEquals(1, prepared); assertEquals(1, api.calls.size)
    }
    @Test fun closingThePickerKeepsAnExplicitlyHandedOffJoinWhileResetRetiresIt() = world {
        load(); var join: CalendarScheduledJoin? = null
        assertTrue(lookup.prepareJoin("occ_one", true, context, "ABCDEFGHJK") { join = it })
        lookup.close(); join!!.requireCurrent(root, "/k/alice")
        assertNull(lookup.state.value.code); assertFalse(lookup.prepareJoin("occ_one", false, context, "ABCDEFGHJK") {})
        lookup.reset()
        try { join!!.requireCurrent(root, "/k/alice"); fail("Expected a retired actor") } catch (_: AuthorizationRetired) { }
        assertTrue(root.isValid)
    }
    @Test fun anOldMailboxStatusCannotReplaceTheCurrentInformation() = world {
        val entered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        api.handler = { call -> if (call.mailbox == "/k/alice") withContext(NonCancellable) { entered.complete(Unit); release.await(); open("occ_old") } else open("occ_new") }
        val old = lookup.load("ABCDEFGHJK", context)!!; entered.await()
        bind("/k/bob"); load("BCDEFGHJKM"); release.complete(Unit); old.join()
        assertEquals("/k/bob", lookup.state.value.mailbox); assertEquals("BCDEFGHJKM", lookup.state.value.code)
        assertEquals("occ_new", lookup.state.value.information!!.choices.single().occurrenceId)
        assertFalse(api.calls.first().lease!!.isValid)
    }
    @Test fun replacingACodeRetiresItsReadEvenThoughTheSameLoginRemainsActive() = world {
        val entered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        api.handler = { call -> if (call.code == "ABCDEFGHJK") withContext(NonCancellable) { entered.complete(Unit); release.await(); open("occ_old") } else open("occ_new") }
        val old = lookup.load("ABCDEFGHJK", context)!!; entered.await()
        val read = api.calls.single().lease!!
        load("BCDEFGHJKM"); assertFalse(read.isValid); assertTrue(root.isValid)
        release.complete(Unit); old.join()
        assertEquals("occ_new", lookup.state.value.information!!.choices.single().occurrenceId)
    }
    @Test fun closingAReadRetiresItsTokenScopeAndDiscardsALateResponse() = world {
        val entered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        api.handler = { withContext(NonCancellable) { entered.complete(Unit); release.await(); open() } }
        val old = lookup.load("ABCDEFGHJK", context)!!; entered.await()
        val read = api.calls.single().lease!!; lookup.close()
        assertFalse(read.isValid); assertTrue(root.isValid)
        release.complete(Unit); old.join()
        assertNull(lookup.state.value.information); assertNull(lookup.state.value.code); assertFalse(lookup.state.value.loading)
    }
    @Test fun malformedWindowsAndClosedStatusGrantNoJoin() = world {
        for (value in listOf(open().copy(state = "not_open"), CalendarMeetingStatus("open", listOf(CalendarMeetingOccurrence("occ_one"))))) {
            api.handler = { value }; load()
            assertFalse(lookup.prepareJoin("occ_one", false, context, "ABCDEFGHJK") {})
        }
    }
    @Test fun backendDetailsStayOutOfErrorsAndOnlyCurrentExpirySignsOut() = world {
        api.handler = { throw IOException("private-synthetic-detail") }; load()
        assertEquals("The meeting could not be loaded. Try again.", lookup.state.value.error)
        api.handler = { throw SessionExpired() }; load()
        assertEquals(1, expired); assertNull(lookup.state.value.mailbox)
    }
    @Test fun aLateExpiryCannotSignOutAReplacementActor() = world {
        val entered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        api.handler = { call -> if (call.mailbox == "/k/alice") withContext(NonCancellable) { entered.complete(Unit); release.await(); throw SessionExpired() } else open() }
        val old = lookup.load("ABCDEFGHJK", context)!!; entered.await()
        bind("/k/bob"); load(); release.complete(Unit); old.join()
        assertEquals(0, expired); assertEquals("/k/bob", lookup.state.value.mailbox)
    }
    @Test fun metadataKeepsTheActorButAnAcceptedLoginChangeRequiresANewContext() = world {
        load(); val old = context
        bind(); assertEquals(old, context); assertNotNull(lookup.state.value.information)
        scope.retire(); scope.activate(AuthorizationLease()); bind()
        assertNotEquals(old, context); assertNull(lookup.state.value.information)
        assertNull(lookup.load("ABCDEFGHJK", old))
    }
    companion object { private fun open(id: String = "occ_one") = CalendarMeetingStatus("open", listOf(CalendarMeetingOccurrence(id, 100, 200, 50, 250))) }
}
