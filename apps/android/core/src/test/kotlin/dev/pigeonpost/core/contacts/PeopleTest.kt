package dev.pigeonpost.core.contacts

import dev.pigeonpost.core.*
import dev.pigeonpost.core.groups.GroupTransport
import kotlinx.coroutines.*
import kotlinx.coroutines.test.*
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

@OptIn(ExperimentalCoroutinesApi::class)
class PeopleTest {
    private class Aliases : LocalAliases {
        val entries = mutableMapOf<String, Map<String, String>>()
        override fun read(subject: String, mailbox: String) = entries["$subject:$mailbox"].orEmpty()
        override fun write(subject: String, mailbox: String, values: Map<String, String>) { entries["$subject:$mailbox"] = values }
        override fun clear() { entries.clear() }
    }
    private class Api : PeopleApi {
        val batches = mutableListOf<List<String>>()
        val invites = mutableListOf<String>()
        var allowance = 1000
        var discoverHook: suspend () -> Unit = {}
        var phonesHook: suspend () -> Unit = {}
        var matches: List<DiscoveryMatch> = emptyList()
        val number = VerifiedPhone("phone_fixture", "/k/mine", "1", "+1 ••• ••• ••00", false, 1)
        override suspend fun phones(mailbox: String): List<VerifiedPhone> { phonesHook(); return listOf(number.copy(identity = mailbox)) }
        override suspend fun sendCode(mailbox: String, phone: String) = PhoneChallenge("ch_fixture", 1600, 1060, 6)
        override suspend fun verify(mailbox: String, challenge: String, code: String, discoverable: Boolean) = number.copy(identity = mailbox, discoverable = discoverable)
        override suspend fun discoverable(mailbox: String, id: String, value: Boolean) = number.copy(discoverable = value)
        override suspend fun unlink(mailbox: String, id: String) {}
        override suspend fun discover(mailbox: String, hashes: List<String>): DiscoveryResult {
            batches += hashes; discoverHook()
            return DiscoveryResult(matches.filter { it.hash in hashes }, hashes.size, hashes.size, allowance)
        }
        override suspend fun invite(mailbox: String, phone: String): InvitationResult { invites += phone; return InvitationResult("sent", id = "inv_fixture") }
        override suspend fun ownName(mailbox: String): ProfileName? = null
        override suspend fun saveName(mailbox: String, name: String, visibility: String) = ProfileName(name, visibility, 1)
        override suspend fun removeName(mailbox: String) {}
        override suspend fun profiles(mailbox: String, addresses: List<String>) = addresses.associateWith { PeerProfile("/k/peer", name = "Marina") }
    }
    private class World(scope: CoroutineScope) {
        var owner = PeopleIdentity("person", AuthorizationLease(), "/k/mine", setOf(Features.PHONE_NUMBERS, Features.CONTACT_DISCOVERY, Features.SMS_INVITES, Features.DISPLAY_NAMES))
        val api = Api(); val local = Aliases()
        val store = PeopleStore(api, scope, { owner }, local, now = { 1000 })
        init { store.synchronize() }
        val context get() = store.state.value.context
        fun replace(mailbox: String = "/k/mine") { owner.authorization.retire(); owner = PeopleIdentity(owner.subject, AuthorizationLease(), mailbox, owner.features); store.synchronize() }
        fun contacts(count: Int) = (0 until count).map { DeviceContact("+1555" + (5550000 + it), "Local name $it") }
    }
    @Test fun hashesMatchServerAndNamesRejectSpoofing() {
        assertEquals("7c0dc19d3eae376a630cee358d0f8912", PeopleInput.hash("+15555550100"))
        assertEquals("5c4fd5bb0f19345b15863f865cb76cee", PeopleInput.hash("+447700900123"))
        assertEquals("511ebc743c8a1d1ea8be5541f2fee95d", PeopleInput.hash("+61491570156"))
        for (bad in listOf("/marina", "@marina", "Admin", "Pigeonpost team", "A\u202eB", "A\u200bB", "A\u3164B", "A\u2800B", "A\ufe0fB", "Pigeon Post", "--")) assertTrue(bad, runCatching { PeopleInput.name(bad) }.isFailure)
        assertEquals("Çağrı", PeopleInput.name(" Çağrı "))
        assertEquals("Support", PeopleInput.name("Support", alias = true))
    }
    @Test fun batchesNeverSpendBeyondNewAccountAllowanceOrInviteUncheckedNumbers() = runTest {
        val w = World(this); w.api.allowance = 0
        val contacts = w.contacts(400); w.store.discover(w.context, contacts)!!.join()
        assertEquals(listOf(200), w.api.batches.map { it.size }); assertEquals(200, w.store.state.value.checked.size)
        assertNull(w.store.reviewInvite(w.context, contacts.last().hash)); assertTrue(w.api.invites.isEmpty())
        assertTrue(w.store.state.value.message!!.contains("200 of 400"))
    }
    @Test fun laterFailureRetainsPartialDiscoveryAndNoAutomaticInvites() = runTest {
        val w = World(this); var calls = 0
        w.api.discoverHook = { if (++calls == 2) throw ApiException(429, "rate_limited") }
        val contacts = w.contacts(401); w.api.matches = listOf(DiscoveryMatch(contacts.first().hash, "/k/peer"))
        w.store.discover(w.context, contacts)!!.join()
        assertEquals(200, w.store.state.value.checked.size); assertEquals(1, w.store.state.value.matches.size)
        assertNull(w.store.reviewInvite(w.context, contacts.first().hash)); assertNull(w.store.reviewInvite(w.context, contacts.last().hash))
        assertTrue(w.api.invites.isEmpty()); assertFalse(w.store.state.value.busy)
    }
    @Test fun invitationRequiresOneCurrentConfirmationAndCannotReplay() = runTest {
        val w = World(this); val contact = w.contacts(1).single(); w.store.discover(w.context, listOf(contact))!!.join()
        val review = w.store.reviewInvite(w.context, contact.hash)!!
        assertTrue(w.api.invites.isEmpty())
        w.store.invite(review)!!.join(); assertEquals(listOf(contact.phone), w.api.invites)
        assertNull(w.store.invite(review)); assertNull(w.store.reviewInvite(w.context, contact.hash))
    }
    @Test fun sameAddressNewLoginInvalidatesImportAndInvitation() = runTest {
        val w = World(this); val contact = w.contacts(1).single(); w.store.discover(w.context, listOf(contact))!!.join()
        val before = w.context; val review = w.store.reviewInvite(before, contact.hash)!!
        w.replace(); assertNotEquals(before, w.context)
        assertNull(w.store.invite(review)); assertNull(w.store.discover(before, listOf(contact)))
        assertTrue(w.store.state.value.imported.isEmpty()); assertTrue(w.api.invites.isEmpty())
    }
    @Test fun delayedResponseCannotPopulateReplacementLogin() = runTest {
        val w = World(this); val gate = CompletableDeferred<Unit>()
        w.api.phonesHook = { withContext(NonCancellable) { gate.await() } }
        val pending = w.store.loadPhones(w.context)!!; runCurrent(); w.replace(); gate.complete(Unit); pending.join()
        assertTrue(w.store.state.value.phones.isEmpty()); assertFalse(w.store.state.value.busy)
    }
    @Test fun mailboxRoundTripAndFeatureWithdrawalInvalidateDialog() = runTest {
        val w = World(this); val first = w.context
        w.owner = PeopleIdentity("person", w.owner.authorization, "/k/other", w.owner.features); w.store.synchronize()
        w.owner = PeopleIdentity("person", w.owner.authorization, "/k/mine", w.owner.features); w.store.synchronize()
        assertNull(w.store.sendCode(first, "+15555550100"))
        w.owner = PeopleIdentity("person", w.owner.authorization, "/k/mine", emptySet()); w.store.synchronize()
        assertNull(w.store.loadPhones(w.context)); assertNull(w.store.discover(w.context, w.contacts(1)))
    }
    @Test fun aliasesAreLocalScopedAndRemovedOnSignout() = runTest {
        val w = World(this); assertTrue(w.store.alias(w.context, "/k/peer", "Marina"))
        assertEquals("Marina", w.store.state.value.label("/k/peer")); assertEquals(1, w.local.entries.size)
        w.replace("/k/other"); assertEquals("peer", w.store.state.value.label("/k/peer"))
        w.store.reset(clearAliases = true); assertTrue(w.local.entries.isEmpty()); assertTrue(w.api.invites.isEmpty())
    }
    @Test fun labelPrecedenceNeverTurnsUnverifiedNameIntoHandle() {
        val s = PeopleState(aliases = mapOf("/k/peer" to "Local"), profiles = mapOf("/k/peer" to PeerProfile("/k/peer", "/marina/main", "Marina")))
        assertEquals("Local", s.label("/k/peer", "Legacy")); assertEquals("Legacy", s.copy(aliases = emptyMap()).label("/k/peer", "Legacy"))
        assertEquals("~Marina", s.copy(aliases = emptyMap()).label("/k/peer"))
    }
    private class Transport(var response: String) : GroupTransport {
        var body: JsonElement? = null; var method: String? = null
        override suspend fun groupJson(mailbox: String, method: String, path: String, body: JsonElement?, device: String?): String { this.body = body; this.method = method; return response }
        override suspend fun groupUpload(mailbox: String, path: String, bytes: ByteArray, device: String) = error("unused")
        override suspend fun groupDownload(mailbox: String, path: String, device: String?, maximumBytes: Long) = error("unused")
    }
    @Test fun wireSendsOnlyHashesAndRejectsUnexpectedMatches() = runTest {
        val hash = PeopleInput.hash("+15555550100")
        val transport = Transport("""{"matches":[],"checked":1,"new":1,"remaining_today":199}""")
        val client = PeopleClient(transport)
        withContext(AuthorizationContext(AuthorizationLease())) {
            assertEquals(199, client.discover("/k/mine", listOf(hash)).remaining)
            assertEquals(setOf("hashes"), transport.body!!.jsonObject.keys)
            assertFalse(transport.body.toString().contains("+1555"))
            transport.response = """{"matches":[{"hash":"${"a".repeat(32)}","address":"/k/peer"}],"checked":1,"new":1,"remaining_today":199}"""
            assertTrue(runCatching { client.discover("/k/mine", listOf(hash)) }.exceptionOrNull() is ApiException)
        }
        assertTrue(runCatching { client.discover("/k/mine", listOf(hash)) }.exceptionOrNull() is AuthorizationRetired)
    }
}
