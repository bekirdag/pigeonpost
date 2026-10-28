package dev.pigeonpost.core.meetings

import dev.pigeonpost.core.ApiException
import dev.pigeonpost.core.AuthorizationContext
import dev.pigeonpost.core.AuthorizationLease
import dev.pigeonpost.core.WireJson
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.StandardTestDispatcher
import kotlinx.coroutines.test.TestScope
import kotlinx.coroutines.test.advanceUntilIdle
import kotlinx.coroutines.test.runTest
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.*
import org.junit.Test
import java.security.KeyFactory
import java.security.KeyPair
import java.security.interfaces.ECPrivateKey
import java.security.spec.PKCS8EncodedKeySpec

@OptIn(ExperimentalCoroutinesApi::class)
class MeetingNotesTest {
    private val vector = WireJson.parseToJsonElement(javaClass.getResource("/meeting-notes-vector.json")!!.readText()).jsonObject
    private val vectorMeeting = WireJson.decodeFromJsonElement(MeetingDetail.serializer(), vector["meeting"]!!)
    private fun vectorKey(): NotesKey {
        val private = KeyFactory.getInstance("EC").generatePrivate(PKCS8EncodedKeySpec(MeetingNotesCrypto.unb64(vector["private_pkcs8"]!!.jsonPrimitive.content))) as ECPrivateKey
        val public = MeetingNotesCrypto.publicFromRaw(MeetingNotesCrypto.unb64(vector["public_key"]!!.jsonPrimitive.content, 65))
        return SoftwareNotesKey(vector["key_id"]!!.jsonPrimitive.content, KeyPair(public, private))
    }

    @Test fun aBriefSealedByMeetingIntelOpensHere() {
        val brief = MeetingBrief.parse(MeetingNotesCrypto.open(vectorMeeting, vectorKey()))
        assertEquals("Sprint planning", brief.title)
        assertEquals(listOf("/alice", "/bob"), brief.participants)
        assertEquals("Ship 0.4.1 to internal testing", brief.decisions.single())
        assertEquals("/bob: Update the Play data safety form (due Friday)", MeetingBrief.actionText(brief.actionItems.single()))
        assertEquals(2, brief.transcript.size)
        assertTrue(brief.asText().contains("[02:10] /bob: I'll update the data safety form by Friday."))
        assertEquals("qwen-latest-flash-next", brief.generatedBy?.llm)
    }

    @Test fun anotherDevicesKeyOrATamperedBriefDoesNotOpen() {
        val stranger = SoftwareNotesKey("nkVectorKey0001")
        assertEquals("bad_ciphertext", assertThrows(MeetingNotesError::class.java) { MeetingNotesCrypto.open(vectorMeeting, stranger) }.code)
        val missing = SoftwareNotesKey("nkNotInTheList1")
        assertEquals("no_key_for_device", assertThrows(MeetingNotesError::class.java) { MeetingNotesCrypto.open(vectorMeeting, missing) }.code)
        val flipped = vectorMeeting.ciphertext!!.let { ct -> MeetingNotesCrypto.b64(MeetingNotesCrypto.unb64(ct).also { it[3] = (it[3].toInt() xor 1).toByte() }) }
        assertEquals("bad_ciphertext", assertThrows(MeetingNotesError::class.java) { MeetingNotesCrypto.open(vectorMeeting.copy(ciphertext = flipped), vectorKey()) }.code)
        // The content key is bound to its meeting: the same ciphertext under another id fails.
        assertThrows(MeetingNotesError::class.java) { MeetingNotesCrypto.open(vectorMeeting.copy(id = "mtg_" + "z".repeat(26)), vectorKey()) }
        // A point off the curve is refused before any key agreement.
        val bad = vectorMeeting.keys.map { it.copy(epk = MeetingNotesCrypto.b64(ByteArray(65).also { b -> b[0] = 4; b[64] = 1 })) }
        assertThrows(MeetingNotesError::class.java) { MeetingNotesCrypto.open(vectorMeeting.copy(keys = bad), vectorKey()) }
    }

    @Test fun sealingHereMatchesTheWebFormatAndRoundTrips() {
        val mine = SoftwareNotesKey(); val other = SoftwareNotesKey()
        val id = "mtg_" + "q".repeat(26)
        val sealed = MeetingNotesCrypto.seal(id, """{"title":"T","summary":"S"}""".toByteArray(),
            listOf(other.keyId to MeetingNotesCrypto.unb64(other.publicKey), mine.keyId to MeetingNotesCrypto.unb64(mine.publicKey)))
        assertEquals(2, sealed.wraps.size)
        sealed.wraps.forEach { assertEquals(65, MeetingNotesCrypto.unb64(it.epk).size); assertEquals(48, MeetingNotesCrypto.unb64(it.wrapped).size) }
        val detail = MeetingDetail(id, state = "ready", alg = "A256GCM", iv = sealed.iv, ciphertext = sealed.ciphertext, keys = sealed.wraps)
        assertEquals("S", MeetingBrief.parse(MeetingNotesCrypto.open(detail, mine)).summary)
        assertEquals("S", MeetingBrief.parse(MeetingNotesCrypto.open(detail, other)).summary)
    }

    @Test fun hkdfMatchesRfc5869() {
        // RFC 5869 test case 1 with UTF-8-only inputs is not expressible; check the construction against a
        // second, independent HMAC computation instead.
        val out = MeetingNotesCrypto.hkdf(ByteArray(32) { 7 }, "mtg_x", "info")
        val mac = { key: ByteArray, data: ByteArray -> javax.crypto.Mac.getInstance("HmacSHA256").run { init(javax.crypto.spec.SecretKeySpec(key, "HmacSHA256")); doFinal(data) } }
        val prk = mac("mtg_x".toByteArray(), ByteArray(32) { 7 })
        assertArrayEquals(mac(prk, "info".toByteArray() + byteArrayOf(1)), out)
    }

    @Test fun keyIdsLookLikeTheWebsAndAreAccepted() {
        val id = MeetingNotesCrypto.newKeyId()
        assertTrue(id.startsWith("nk") && id.length == 18 && MeetingNotesCrypto.validKeyId(id))
        assertFalse(MeetingNotesCrypto.validKeyId("short")); assertFalse(MeetingNotesCrypto.validKeyId("nk/../../x"))
    }

    // ---- the store ------------------------------------------------------------------------------

    private class FakeApi(val key: NotesKey) : MeetingNotesApi {
        val registered = mutableListOf<Pair<String, String>>()
        val deleted = mutableListOf<String>()
        var rows = mutableListOf<MeetingRow>()
        val details = mutableMapOf<String, MeetingDetail>()
        var failList = false
        override suspend fun meetings() = if (failList) throw ApiException(500, "boom") else rows.toList()
        override suspend fun meeting(id: String) = details[id] ?: throw ApiException(404, "not_found")
        override suspend fun delete(id: String) { deleted += id; rows.removeAll { it.id == id } }
        override suspend fun registerKey(keyId: String, publicKey: String, label: String) { registered += keyId to label }
    }

    private fun TestScope.store(api: FakeApi, key: NotesKey?) =
        MeetingNotesStore(api, { key }, kotlinx.coroutines.CoroutineScope(coroutineContext + AuthorizationContext(AuthorizationLease())),
            crypto = kotlinx.coroutines.test.StandardTestDispatcher(testScheduler))

    @Test fun theStoreRegistersThisDevicesKeyListsAndOpens() = runTest(StandardTestDispatcher()) {
        val mine = SoftwareNotesKey()
        val api = FakeApi(mine)
        val ready = "mtg_" + "r".repeat(26); val failed = "mtg_" + "f".repeat(26); val elsewhere = "mtg_" + "e".repeat(26)
        val sealed = MeetingNotesCrypto.seal(ready, """{"title":"Weekly","summary":"Fine.","decisions":["Yes"]}""".toByteArray(), listOf(mine.keyId to MeetingNotesCrypto.unb64(mine.publicKey)))
        val forOthers = MeetingNotesCrypto.seal(elsewhere, "{}".toByteArray(), listOf(SoftwareNotesKey().let { it.keyId to MeetingNotesCrypto.unb64(it.publicKey) }))
        api.rows = mutableListOf(MeetingRow(ready, state = "ready", createdAt = 30), MeetingRow(failed, state = "failed", createdAt = 20, failure = "no_speech"),
            MeetingRow(elsewhere, state = "ready", createdAt = 10))
        api.details[ready] = MeetingDetail(ready, state = "ready", alg = "A256GCM", iv = sealed.iv, ciphertext = sealed.ciphertext, keys = sealed.wraps)
        api.details[failed] = MeetingDetail(failed, state = "failed", failure = "no_speech")
        api.details[elsewhere] = MeetingDetail(elsewhere, state = "ready", alg = "A256GCM", iv = forOthers.iv, ciphertext = forOthers.ciphertext, keys = forOthers.wraps)
        val s = store(api, mine)
        s.onMailbox("/k/me", setOf("meeting_summaries")); advanceUntilIdle()
        assertEquals(listOf(mine.keyId to "Android app"), api.registered)
        assertTrue(s.state.value.registered)
        s.load(); advanceUntilIdle()
        assertEquals(listOf(ready, failed, elsewhere), s.state.value.meetings.map { it.id })
        s.open(s.state.value.meetings[0]); advanceUntilIdle()
        assertEquals("Weekly", s.state.value.opened?.brief?.title)
        s.open(s.state.value.meetings[1]); advanceUntilIdle()
        assertEquals("No speech was captured.", s.state.value.opened?.problem)
        s.open(s.state.value.meetings[2]); advanceUntilIdle()
        assertEquals(MeetingNotesText.NO_KEY, s.state.value.opened?.problem)
        s.delete(ready); advanceUntilIdle()
        assertEquals(listOf(ready), api.deleted)
        assertEquals(listOf(failed, elsewhere), s.state.value.meetings.map { it.id })
        assertNull(s.state.value.opened)
    }

    @Test fun offOrSignedOutTheStoreDoesNothing() = runTest(StandardTestDispatcher()) {
        val api = FakeApi(SoftwareNotesKey())
        val s = store(api, api.key)
        s.onMailbox("/k/me", setOf("groups")); advanceUntilIdle()
        assertFalse(s.state.value.enabled); assertTrue(api.registered.isEmpty())
        s.load(); advanceUntilIdle()
        assertTrue(s.state.value.meetings.isEmpty())
        s.onMailbox("/k/me", setOf("meeting_summaries")); advanceUntilIdle()
        api.failList = true
        s.load(); advanceUntilIdle()
        assertEquals("Couldn’t load your meeting notes.", s.state.value.error)
        s.onMailbox(null, emptySet()); advanceUntilIdle()
        assertFalse(s.state.value.enabled); assertNull(s.state.value.error)
    }
}
