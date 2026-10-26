package dev.pigeonpost.core.groups

import dev.pigeonpost.core.ApiException
import kotlinx.coroutines.async
import kotlinx.coroutines.awaitAll
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.yield
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.put
import java.io.IOException
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.BeforeClass
import org.junit.Test

/**
 * The Android client loop (GroupClient) with the real group-crypto library, against an in-memory
 * postbox that enforces docs/groups.md: activation, invitations, messaging, attachments, removal,
 * a new device without history, concurrent commits, epoch conflicts, persistence and expiry.
 * The same scenarios as the web inbox's site-inbox/test/groups-mls.test.mjs.
 */
class GroupClientMlsTest {
    companion object {
        private var library: UniffiGroupCrypto? = null
        @BeforeClass @JvmStatic fun load() {
            library = try { UniffiGroupCrypto().also { it.generateStateKey() } } catch (failure: Throwable) {
                if (System.getProperty("groupcrypto.required") == "1") throw AssertionError("The host group-crypto library is required but did not load", failure)
                null
            }
        }
    }
    private val crypto get() = library!!

    private class World(val fake: FakeGroupPostbox, val crypto: UniffiGroupCrypto) {
        suspend fun client(mailbox: String, storage: GroupStorage = MemoryGroupStorage(), now: () -> Long = { System.currentTimeMillis() / 1000 }) =
            GroupClient(crypto, fake.transport, storage, mailbox, now = now, sleep = {}, jitter = { 0 }).also { it.init(); it.topUpKeyPackages() }
    }
    private class Three(val w: World, val a: String, val b: String, val c: String, val alice: GroupClient, val bob: GroupClient, val carol: GroupClient, val gid: String)

    private fun world(): World { assumeTrue("host group-crypto library not built", library != null); return World(FakeGroupPostbox(), crypto) }
    private fun texts(c: GroupClient, gid: String) = c.history(gid).filter { it.kind == "message" && !it.deleted && !it.undecryptable }.map { it.text }

    private suspend fun three(): Three {
        val w = world()
        val (a, b, c) = listOf(w.fake.mailbox(), w.fake.mailbox(), w.fake.mailbox())
        val alice = w.client(a); val bob = w.client(b); val carol = w.client(c)
        val gid = alice.createGroup(buildJsonObject { put("name", "Garden") }).group.id
        w.fake.invite(gid, b); w.fake.invite(gid, c); w.fake.accept(gid, b); w.fake.accept(gid, c)
        alice.processPending(gid, noJitter = true)
        assertEquals(listOf(gid), bob.processWelcomes())
        assertEquals(listOf(gid), carol.processWelcomes())
        return Three(w, a, b, c, alice, bob, carol, gid)
    }

    private suspend fun actionGroup(w: World, alice: GroupClient, bob: GroupClient): String {
        val gid = alice.createGroup(buildJsonObject { put("name", "Actions") }).group.id
        w.fake.invite(gid, bob.mailbox); w.fake.accept(gid, bob.mailbox)
        alice.processPending(gid, noJitter = true); bob.processWelcomes()
        return gid
    }
    private suspend fun expectLoss(work: suspend () -> Unit) {
        try { work(); fail("Expected lost transport") } catch (failure: IOException) { assertFalse(failure is ApiException) }
    }

    @Test fun authenticatedEditsAndRepliesKeepTheOriginalAttachmentKeys() = runBlocking {
        val t = three()
        val root = t.alice.send(t.gid, OutgoingDocument("Original", listOf(OutgoingFile("plan.txt", "text/plain", "Private plan".toByteArray()))))
        t.bob.sync(t.gid)
        t.bob.send(t.gid, OutgoingDocument("Answer", replyTo = root.seq)); t.alice.sync(t.gid)
        assertTrue(t.alice.history(t.gid).single { it.text == "Answer" }.actionsVerified)
        val changed = t.alice.send(t.gid, OutgoingDocument("Updated", editOf = root.seq))
        t.bob.sync(t.gid)
        val edit = t.bob.history(t.gid).single { it.seq == changed.seq && it.kind == "message" }
        assertTrue(edit.actionsVerified)
        assertEquals(listOf("Original", "Updated"), GroupActions.versions(edit, t.bob.history(t.gid)).map { it.text })
        assertEquals(listOf("Answer", "Updated"), GroupActions.visible(t.bob.history(t.gid), System.currentTimeMillis() / 1000).filter { it.kind == "message" }.map { it.text })
        assertEquals("Private plan", String(t.bob.openFile(t.gid, edit.files.single())))
    }

    @Test fun forgedServerReferencesAndReplacementHintsCannotHideRealMlsPlaintext() = runBlocking {
        val t = three()
        val root = t.alice.send(t.gid, OutgoingDocument("Original"))
        val second = t.alice.send(t.gid, OutgoingDocument("Another ordinary message"))
        val items = t.w.fake.groups.getValue(t.gid).items
        for (i in items.indices) {
            when (items[i]["seq"]) {
                JsonPrimitive(root.seq) -> items[i] = JsonObject(items[i] + ("replaced_by" to JsonPrimitive(second.seq)))
                JsonPrimitive(second.seq) -> items[i] = JsonObject(items[i] + ("edit_of" to JsonPrimitive(root.seq)))
                else -> Unit
            }
        }
        t.bob.sync(t.gid)
        assertEquals(listOf("Original", "Another ordinary message"), GroupActions.visible(t.bob.history(t.gid), System.currentTimeMillis() / 1000).filter { it.kind == "message" }.map { it.text })
        assertTrue(t.bob.history(t.gid).any { it.warning == "action_mismatch" })
        assertNull(t.bob.history(t.gid).single { it.text == "Another ordinary message" }.editOf)
    }

    @Test fun theVerifiedMlsDeviceMustMatchTheSequencedSenderDevice() = runBlocking {
        val t = three()
        val posted = t.alice.send(t.gid, OutgoingDocument("Authenticated author"))
        val items = t.w.fake.groups.getValue(t.gid).items
        val index = items.indexOfFirst { it["seq"] == JsonPrimitive(posted.seq) }
        items[index] = JsonObject(items[index] + ("sender_device" to JsonPrimitive("forged-device")))
        t.bob.sync(t.gid)
        assertTrue(t.bob.history(t.gid).any { it.warning == "sender_mismatch" })
        assertFalse(t.bob.history(t.gid).any { it.text == "Authenticated author" })
    }

    @Test fun aCommittedResponseLossSurvivesRestartWithTheExactEncryptedRequest() = runBlocking {
        val w = world(); val a = w.fake.mailbox(); val b = w.fake.mailbox(); val storage = MemoryGroupStorage()
        var lose = true
        val attempts = mutableListOf<String>()
        val wire = object : GroupTransport by w.fake.transport {
            override suspend fun groupJson(mailbox: String, method: String, path: String, body: JsonElement?, device: String?): String {
                val response = w.fake.transport.groupJson(mailbox, method, path, body, device)
                if (method == "POST" && path.endsWith("/messages") && mailbox == a) {
                    attempts += body.toString()
                    if (lose) { lose = false; throw IOException("Fixture committed response loss") }
                }
                return response
            }
        }
        val alice = GroupClient(crypto, wire, storage, a, sleep = {}, jitter = { 0 }).also { it.init(); it.topUpKeyPackages() }
        val bob = w.client(b); val gid = actionGroup(w, alice, bob)
        expectLoss { alice.send(gid, OutgoingDocument("Only once")) }
        assertEquals(1, attempts.size)
        val pending = storage.rows.getValue(a).pendingSends.values.single()
        assertEquals(attempts.single(), pending.request)
        val restarted = GroupClient(crypto, wire, storage, a, sleep = {}, jitter = { 0 }).also { it.init() }
        assertEquals("Restoring never automatically sends", 1, attempts.size)
        assertTrue(restarted.history(gid).single { it.cmid == pending.cmid }.sendFailed)
        restarted.retry(gid, pending.cmid)
        assertEquals(2, attempts.size); assertEquals(attempts[0], attempts[1])
        assertTrue(storage.rows.getValue(a).pendingSends.isEmpty())
        bob.sync(gid); assertEquals(listOf("Only once"), texts(bob, gid))
        assertEquals(1, w.fake.groups.getValue(gid).items.count { it["type"] == JsonPrimitive("application") })
    }

    @Test fun losingAnEditResponseDoesNotEncryptOrCreateAnotherRevisionOnRetry() = runBlocking {
        val w = world(); val a = w.fake.mailbox(); val b = w.fake.mailbox(); val storage = MemoryGroupStorage()
        var lose = false
        val attempts = mutableListOf<String>()
        val wire = object : GroupTransport by w.fake.transport {
            override suspend fun groupJson(mailbox: String, method: String, path: String, body: JsonElement?, device: String?): String {
                val response = w.fake.transport.groupJson(mailbox, method, path, body, device)
                if (method == "POST" && path.endsWith("/messages") && mailbox == a && body.toString().contains("edit_of")) {
                    attempts += body.toString()
                    if (lose) { lose = false; throw IOException("Fixture lost edit response") }
                }
                return response
            }
        }
        val alice = GroupClient(crypto, wire, storage, a, sleep = {}, jitter = { 0 }).also { it.init(); it.topUpKeyPackages() }
        val bob = w.client(b); val gid = actionGroup(w, alice, bob)
        val root = alice.send(gid, OutgoingDocument("Before")); lose = true
        expectLoss { alice.send(gid, OutgoingDocument("After", editOf = root.seq)) }
        val pending = storage.rows.getValue(a).pendingSends.values.single()
        val restarted = GroupClient(crypto, wire, storage, a, sleep = {}, jitter = { 0 }).also { it.init() }
        restarted.retry(gid, pending.cmid)
        assertEquals(2, attempts.size); assertEquals(attempts[0], attempts[1])
        bob.sync(gid)
        val edited = bob.history(gid).single { it.text == "After" }
        assertEquals(listOf("Before", "After"), GroupActions.versions(edited, bob.history(gid)).map { it.text })
        assertEquals(2, w.fake.groups.getValue(gid).items.count { it["type"] == JsonPrimitive("application") })
    }

    @Test fun deletingAChainAlsoErasesUnconfirmedDependentEditsAndAttachmentKeys() = runBlocking {
        val w = world(); val a = w.fake.mailbox(); val b = w.fake.mailbox(); val storage = MemoryGroupStorage()
        var rejectTransport = false
        val wire = object : GroupTransport by w.fake.transport {
            override suspend fun groupJson(mailbox: String, method: String, path: String, body: JsonElement?, device: String?): String {
                if (rejectTransport && method == "POST" && path.endsWith("/messages")) throw IOException("Fixture unavailable before delivery")
                return w.fake.transport.groupJson(mailbox, method, path, body, device)
            }
        }
        val alice = GroupClient(crypto, wire, storage, a, sleep = {}, jitter = { 0 }).also { it.init(); it.topUpKeyPackages() }
        val bob = w.client(b); val gid = actionGroup(w, alice, bob)
        val root = alice.send(gid, OutgoingDocument("Secret", listOf(OutgoingFile("secret.txt", "text/plain", "Secret file".toByteArray()))))
        val edit = alice.send(gid, OutgoingDocument("Changed secret", editOf = root.seq))
        rejectTransport = true
        expectLoss { alice.send(gid, OutgoingDocument("Unconfirmed secret", editOf = edit.seq)) }
        val cmid = storage.rows.getValue(a).pendingSends.keys.single()
        alice.dropSeqs(gid, listOf(root.seq), tombstones = true)
        assertTrue(storage.rows.getValue(a).pendingSends.isEmpty())
        assertTrue(alice.history(gid).filter { it.kind == "message" }.all { it.deleted && it.text == null && it.files.isEmpty() })
        try { alice.retry(gid, cmid); fail("Deleted edit retried") } catch (failure: ApiException) { assertEquals("send_unavailable", failure.code) }
    }

    @Test fun createActivatesAndAcceptedInvitationsBecomeEncryptedMembers() = runBlocking {
        val t = three()
        val g = t.w.fake.groups.getValue(t.gid)
        assertEquals("active", g.activation)
        assertEquals("activation commit, then one add commit for both invitees", 2L, g.epoch)
        assertEquals(2L, t.alice.epoch(t.gid)); assertEquals(2L, t.bob.epoch(t.gid))
        assertEquals("everyone sees the same device list", t.alice.safetyNumber(t.gid), t.carol.safetyNumber(t.gid))
        assertEquals(3, t.alice.members(t.gid).size)
        assertTrue("the joining device records that earlier messages are not here", t.bob.history(t.gid).any { it.notice == "you_joined" })
    }

    @Test fun messagesAndAttachmentsRoundTripAndTheServerHoldsOnlyCiphertext() = runBlocking {
        val t = three()
        t.alice.send(t.gid, OutgoingDocument("hello garden"))
        t.bob.sync(t.gid); t.carol.sync(t.gid)
        assertEquals(listOf("hello garden"), texts(t.bob, t.gid))
        assertEquals(listOf("hello garden"), texts(t.carol, t.gid))
        val stored = t.w.fake.groups.getValue(t.gid).items.first { it["type"].toString() == "\"application\"" }
        assertFalse(String(java.util.Base64.getDecoder().decode(stored["ciphertext"]!!.toString().trim('"')), Charsets.ISO_8859_1).contains("hello garden"))

        t.bob.send(t.gid, OutgoingDocument("see attached", listOf(OutgoingFile("plan.txt", "text/plain", "a secret file".toByteArray()))))
        t.carol.sync(t.gid)
        val message = t.carol.history(t.gid).first { it.text == "see attached" }
        assertEquals("plan.txt", message.files.single().name)
        assertEquals("a secret file", String(t.carol.openFile(t.gid, message.files.single())))
        assertFalse("attachment stored sealed", String(t.w.fake.blobs.getValue(message.files.single().id), Charsets.ISO_8859_1).contains("secret"))
        // The sender keeps its own copy without decrypting it again.
        assertEquals(listOf("hello garden", "see attached"), texts(t.bob, t.gid))
    }

    @Test fun aRemovedMemberLosesAccessAndCannotDecryptLaterMessages() = runBlocking {
        val t = three()
        t.alice.send(t.gid, OutgoingDocument("before"))
        t.carol.sync(t.gid)
        t.w.fake.remove(t.gid, t.c)
        // The pending removal blocks application sends until it is committed; the client commits it.
        t.alice.send(t.gid, OutgoingDocument("after"))
        assertTrue(t.w.fake.calls.any { it.third.endsWith("/commits") })
        assertFalse(t.w.fake.groups.getValue(t.gid).leaves.contains(t.c + "|" + t.carol.deviceId))
        t.bob.sync(t.gid)
        assertEquals(listOf("before", "after"), texts(t.bob, t.gid))
        try { t.carol.sync(t.gid); fail("removed member still reads") } catch (e: ApiException) { assertEquals("not_member", e.code) }
        assertEquals(listOf("before"), texts(t.carol, t.gid))
    }

    @Test fun aNewDeviceOfAnExistingMemberJoinsWithoutHistoryAndIsVisibleToOthers() = runBlocking {
        val t = three()
        t.alice.send(t.gid, OutgoingDocument("old news"))
        val bob2 = t.w.client(t.b)
        assertNotEquals(bob2.deviceId, t.bob.deviceId)
        t.bob.processPending(t.gid, noJitter = true) // a device of the same mailbox adds it
        assertEquals(listOf(t.gid), bob2.processWelcomes())
        t.alice.send(t.gid, OutgoingDocument("fresh"))
        bob2.sync(t.gid)
        assertEquals("no pre-join history", listOf("fresh"), texts(bob2, t.gid))
        t.alice.sync(t.gid)
        val commit = t.alice.history(t.gid).last { it.kind == "commit" && it.added.any { d -> d.deviceId == bob2.deviceId } }
        val lines = commitLines(t.alice.history(t.gid), commit, t.a, t.alice.deviceId) { it ?: "?" }
        assertEquals("the new device is announced as a device change, with its fingerprint", listOf(true), lines.map { it.security })
        assertTrue(lines.single().text.endsWith("added a new device"))
        assertEquals(64, lines.single().fingerprint!!.length)
    }

    @Test fun concurrentCommitsTheFirstWinsAndTheOtherRecovers() = runBlocking {
        val t = three()
        val d = t.w.fake.mailbox()
        val dave = t.w.client(d)
        t.w.fake.invite(t.gid, d); t.w.fake.accept(t.gid, d)
        val before = t.w.fake.groups.getValue(t.gid).epoch
        t.w.fake.beforeHandle = { _, _, _ -> yield() } // interleave the two devices at every request
        listOf(async { t.alice.processPending(t.gid, noJitter = true) }, async { t.bob.processPending(t.gid, noJitter = true) }).awaitAll()
        t.w.fake.beforeHandle = null
        assertEquals("exactly one add commit landed", before + 1, t.w.fake.groups.getValue(t.gid).epoch)
        assertEquals(listOf(t.gid), dave.processWelcomes())
        t.carol.send(t.gid, OutgoingDocument("welcome dave"))
        for (c in listOf(t.alice, t.bob, dave)) c.sync(t.gid)
        assertEquals(listOf("welcome dave"), texts(dave, t.gid))
        assertEquals(listOf("welcome dave"), texts(t.alice, t.gid))
        assertEquals(t.alice.epoch(t.gid), t.bob.epoch(t.gid))
    }

    @Test fun anEpochConflictIsReEncryptedUnderTheSameClientMessageId() = runBlocking {
        val t = three()
        var raced = false
        t.w.fake.beforeHandle = { me, method, path ->
            if (!raced && me == t.a && method == "POST" && path.endsWith("/messages")) { raced = true; t.bob.selfUpdate(t.gid) }
        }
        t.alice.send(t.gid, OutgoingDocument("racing"))
        t.w.fake.beforeHandle = null
        assertTrue("the first post was refused", t.w.fake.errors.any { it.second == "epoch_conflict" })
        assertEquals(1, t.w.fake.groups.getValue(t.gid).items.count { it["type"].toString() == "\"application\"" })
        t.bob.sync(t.gid); t.carol.sync(t.gid)
        assertEquals(listOf("racing"), texts(t.bob, t.gid))
        assertEquals(listOf("racing"), texts(t.carol, t.gid))
    }

    @Test fun stateSurvivesARestartThroughStorage() = runBlocking {
        val w = world()
        val (a, b) = listOf(w.fake.mailbox(), w.fake.mailbox())
        val storage = MemoryGroupStorage()
        val alice = w.client(a, storage)
        val bob = w.client(b)
        val gid = alice.createGroup(buildJsonObject { put("name", "Persist") }).group.id
        w.fake.invite(gid, b); w.fake.accept(gid, b)
        alice.processPending(gid, noJitter = true)
        bob.processWelcomes()
        bob.send(gid, OutgoingDocument("one"))
        alice.sync(gid)
        // The app restarts: a new client over the same storage.
        val restarted = GroupClient(crypto, w.fake.transport, storage, a, sleep = {}, jitter = { 0 })
        assertEquals("same device, not a re-enrolment", alice.deviceId, restarted.init())
        assertEquals(listOf("one"), texts(restarted, gid))
        bob.send(gid, OutgoingDocument("two")); restarted.sync(gid)
        assertEquals(listOf("one", "two"), texts(restarted, gid))
        restarted.send(gid, OutgoingDocument("three")); bob.sync(gid)
        assertEquals("bob decrypts the restarted device", listOf("one", "two", "three"), texts(bob, gid))
    }

    @Test fun expiredItemsAreDroppedByTheDevicesOwnClock() = runBlocking {
        val t = three()
        t.w.fake.groups.getValue(t.gid).ephemeral = 60
        t.alice.send(t.gid, OutgoingDocument("brief"))
        t.w.fake.groups.getValue(t.gid).ephemeral = 0
        t.alice.send(t.gid, OutgoingDocument("kept"))
        var clock = System.currentTimeMillis() / 1000
        val storage = MemoryGroupStorage()
        val bob2 = t.w.client(t.b, storage) { clock }
        t.bob.processPending(t.gid, noJitter = true); bob2.processWelcomes()
        t.alice.send(t.gid, OutgoingDocument("x").copy())
        t.w.fake.groups.getValue(t.gid).ephemeral = 60
        t.alice.send(t.gid, OutgoingDocument("short-lived"))
        bob2.sync(t.gid)
        assertEquals(listOf("x", "short-lived"), texts(bob2, t.gid))
        clock += 61
        assertEquals(1, bob2.pruneExpired())
        assertEquals(listOf("x"), texts(bob2, t.gid))
        val seq = bob2.history(t.gid).first { it.text == "x" }.seq!!
        bob2.dropSeqs(t.gid, listOf(seq))
        assertEquals("message.expired drops by seq", emptyList<String?>(), texts(bob2, t.gid))
    }

    @Test fun revokingThisDeviceRemovesItsKeysAndAFreshDeviceIsEnrolled() = runBlocking {
        val w = world()
        val a = w.fake.mailbox()
        val storage = MemoryGroupStorage()
        val alice = w.client(a, storage)
        val first = alice.deviceId
        alice.revoke()
        assertNull(storage.rows[a])
        assertTrue(w.fake.devices.getValue(a).getValue(first!!).revoked)
        val again = GroupClient(crypto, w.fake.transport, storage, a, sleep = {}, jitter = { 0 })
        assertNotEquals(first, again.init())
    }
}
