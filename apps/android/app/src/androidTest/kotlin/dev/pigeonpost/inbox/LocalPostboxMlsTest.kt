package dev.pigeonpost.inbox

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import dev.pigeonpost.core.PostboxClient
import dev.pigeonpost.core.TokenProvider
import dev.pigeonpost.core.WireJson
import dev.pigeonpost.core.groups.*
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray
import okhttp3.HttpUrl.Companion.toHttpUrl
import org.junit.Assert.*
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith

/**
 * An MLS round trip on the device against a real postbox (tests/e2e-groups/android-postbox.mjs):
 * two hosted mailboxes, each with an Android device built on the AAR, register (the postbox signs
 * the attestations), create a group, invite and accept, add the device with a Welcome, exchange
 * messages and a sealed attachment, and agree on the safety number. Skipped unless the harness's
 * JSON is on the device (see below).
 */
@RunWith(AndroidJUnit4::class)
class LocalPostboxMlsTest {
    @Test fun twoAndroidDevicesTalkThroughARealPostbox() = runBlocking {
        // The harness's printed JSON, pushed to the app's external files directory (long tokens do not
        // survive instrumentation arguments): adb push harness.json /sdcard/Android/data/dev.pigeonpost.inbox.debug/files/postbox.json
        val file = java.io.File(InstrumentationRegistry.getInstrumentation().targetContext.getExternalFilesDir(null), "postbox.json")
        assumeTrue("start tests/e2e-groups/android-postbox.mjs and push its JSON", file.isFile)
        val args = WireJson.parseToJsonElement(file.readText()).let { it as kotlinx.serialization.json.JsonObject }.mapValues { (_, v) -> (v as JsonPrimitive).content }
        val postbox = args.getValue("postbox")
        fun client(token: String) = PostboxClient(object : TokenProvider { override suspend fun token(rejected: String?) = token },
            "$postbox/".toHttpUrl(), allowLoopbackForTests = true)
        val aliceApi = client(args.getValue("aliceToken")); val bobApi = client(args.getValue("bobToken"))
        val a = aliceApi.identities().single().address; val b = bobApi.identities().single().address
        val crypto = UniffiGroupCrypto()
        val alice = GroupClient(crypto, aliceApi, MemoryGroupStorage(), a, jitter = { 0 })
        val bob = GroupClient(crypto, bobApi, MemoryGroupStorage(), b, jitter = { 0 })
        alice.init(); bob.init()
        assertTrue(alice.topUpKeyPackages() >= 10); assertTrue(bob.topUpKeyPackages() >= 10)

        val created = alice.createGroup(buildJsonObject { put("name", "Android round trip"); putJsonArray("invite") { add(JsonPrimitive(b)) } })
        val gid = created.group.id
        assertEquals("invited", created.invitations!!.single().status)
        bobApi.groupJson(b, "POST", "/v1/groups/$gid/invitation", buildJsonObject { put("accept", true) })
        assertNotNull("Alice's device adds Bob's", alice.processPending(gid, noJitter = true))
        assertEquals(listOf(gid), bob.processWelcomes())

        alice.send(gid, OutgoingDocument("hello from Android"))
        bob.sync(gid)
        assertEquals("hello from Android", bob.history(gid).last { it.kind == "message" }.text)
        bob.send(gid, OutgoingDocument("and back", listOf(OutgoingFile("note.txt", "text/plain", "sealed on Android".toByteArray()))))
        alice.sync(gid)
        val reply = alice.history(gid).last { it.kind == "message" }
        assertEquals("and back", reply.text)
        assertEquals("sealed on Android", String(alice.openFile(gid, reply.files.single())))
        assertEquals("both devices see the same device list", alice.safetyNumber(gid), bob.safetyNumber(gid))
        val group = WireJson.decodeFromString<Group>(aliceApi.groupJson(a, "GET", "/v1/groups/$gid"))
        assertEquals("active", group.activationState)
        assertEquals(2, group.memberCount)
    }
}
