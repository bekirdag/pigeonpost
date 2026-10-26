package dev.pigeonpost.core.groups

import dev.pigeonpost.core.ApiException
import dev.pigeonpost.core.PostboxClient
import dev.pigeonpost.core.TokenProvider
import dev.pigeonpost.core.WireJson
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import okhttp3.HttpUrl.Companion.toHttpUrl
import java.io.File
import java.io.IOException
import java.net.URI
import java.net.http.HttpClient
import java.net.http.HttpRequest
import java.net.http.HttpResponse
import kotlin.system.exitProcess

/** Explicit JavaExec target: real Kotlin MLS, HTTP, Rust postbox and disposable accounts only. */
object GroupActionsPostbox {
    private var stage = "setup"
    private val controls = HttpClient.newBuilder().followRedirects(HttpClient.Redirect.NEVER).build()
    private fun verify(value: Boolean, label: String) { if (!value) throw AssertionError(label) }
    private fun origin(value: String): String {
        val url = value.toHttpUrl()
        require(url.scheme == "http" && url.host == "127.0.0.1" && url.encodedPath == "/")
        require(url.username.isEmpty() && url.password.isEmpty() && url.query == null && url.fragment == null)
        return value.trimEnd('/')
    }
    private fun api(base: String, credential: String) = PostboxClient(object : TokenProvider {
        override suspend fun token(rejected: String?) = credential
    }, "$base/".toHttpUrl(), allowLoopbackForTests = true)
    private fun control(base: String, path: String, mode: String? = null): JsonObject {
        val request = HttpRequest.newBuilder(URI.create(base + path))
        if (mode != null) request.header("content-type", "application/json").POST(HttpRequest.BodyPublishers.ofString(buildJsonObject { put("mode", mode) }.toString()))
        val response = controls.send(request.build(), HttpResponse.BodyHandlers.ofString())
        verify(response.statusCode() == 200, "Loopback control failed")
        return WireJson.parseToJsonElement(response.body()).jsonObject
    }
    private fun mode(proxy: String, value: String) { control(proxy, "/__test/control", value) }
    private fun attempts(proxy: String) = control(proxy, "/__test/attempts")["attempts"]!!.jsonArray.map { it.jsonObject }
    private suspend fun expectLoss(work: suspend () -> Unit) {
        try { work() } catch (failure: IOException) { verify(failure !is ApiException, "Expected committed transport loss, not server refusal"); return }
        throw AssertionError("Committed response was not dropped")
    }
    private fun sameAttempts(proxy: String, cmid: String) {
        val attempts = attempts(proxy)
        verify(attempts.size == 2 && attempts.map { it["sha"] }.distinct().size == 1, "Explicit retry changed encrypted request bytes")
        verify(attempts.all { it["cmid"]?.jsonPrimitive?.content == cmid && it["status"]!!.jsonPrimitive.int in 200..299 }, "Retry changed client ID or did not follow an accepted commit")
    }
    @JvmStatic fun main(args: Array<String>) {
        try {
            require(args.size == 1)
            runBlocking { qualify(WireJson.parseToJsonElement(File(args.single()).readText()).jsonObject) }
            println("PASS: real Android MLS group actions, files, receipts, restart/retry and deletion reconciliation.")
            exitProcess(0)
        } catch (failure: Throwable) {
            System.err.println("FAIL: group-action interop at $stage (${failure.javaClass.simpleName}).")
            if (failure is AssertionError) System.err.println(failure.message)
            if (failure is ApiException) System.err.println("Postbox status ${failure.status}, code ${failure.code}")
            exitProcess(1)
        }
    }
    private suspend fun qualify(setup: JsonObject) {
        val base = origin(setup["postbox"]!!.jsonPrimitive.content)
        val proxy = origin(setup["lossProxy"]!!.jsonPrimitive.content)
        val people = setup["people"]!!.jsonObject
        val alicePerson = people["alice"]!!.jsonObject
        val bobPerson = people["bob"]!!.jsonObject
        val a = alicePerson["address"]!!.jsonPrimitive.content
        val b = bobPerson["address"]!!.jsonPrimitive.content
        val aliceApi = api(base, alicePerson["token"]!!.jsonPrimitive.content)
        val bobApi = api(base, bobPerson["token"]!!.jsonPrimitive.content)
        val lossyApi = api(proxy, alicePerson["token"]!!.jsonPrimitive.content)
        val crypto = UniffiGroupCrypto().also { it.generateStateKey() }
        val storage = MemoryGroupStorage()
        var alice = GroupClient(crypto, aliceApi, storage, a, sleep = {}, jitter = { 0 })
        val bob = GroupClient(crypto, bobApi, MemoryGroupStorage(), b, sleep = {}, jitter = { 0 })
        stage = "real device registration and encrypted membership"
        alice.init(); bob.init(); alice.topUpKeyPackages(); bob.topUpKeyPackages()
        aliceApi.setPolicy(a, "read_receipts", "true"); bobApi.setPolicy(b, "read_receipts", "true")
        val gid = alice.createGroup(buildJsonObject { put("name", "Local Android action check"); putJsonArray("invite") { add(JsonPrimitive(b)) } }).group.id
        bobApi.groupJson(b, "POST", "/v1/groups/$gid/invitation", buildJsonObject { put("accept", true) })
        verify(alice.processPending(gid, noJitter = true) != null, "Member addition did not commit")
        verify(bob.processWelcomes() == listOf(gid), "Recipient did not verify the Welcome")
        verify(alice.safetyNumber(gid) == bob.safetyNumber(gid), "Devices disagree on verified membership")

        stage = "encrypted reply, versions and retained sealed file"
        val root = alice.send(gid, OutgoingDocument("Original group text", listOf(OutgoingFile("plan.txt", "text/plain", "Private group file".toByteArray()))))
        bob.sync(gid)
        bob.send(gid, OutgoingDocument("Reply from Bob", replyTo = root.seq)); alice.sync(gid)
        verify(alice.history(gid).single { it.text == "Reply from Bob" }.let { it.actionsVerified && it.replyTo == root.seq.toString() }, "Reply references were not authenticated")
        val edited = alice.send(gid, OutgoingDocument("Updated group text", editOf = root.seq)); bob.sync(gid)
        val recipientEdit = bob.history(gid).single { it.kind == "message" && it.seq == edited.seq }
        verify(recipientEdit.actionsVerified, "Edited references did not match the real envelope")
        verify(GroupActions.versions(recipientEdit, bob.history(gid)).map { it.text } == listOf("Original group text", "Updated group text"), "Local original/version history changed")
        verify(String(bob.openFile(gid, recipientEdit.files.single())) == "Private group file", "Text edit lost its original sealed attachment key")
        verify(GroupActions.visible(bob.history(gid), System.currentTimeMillis() / 1000).count { it.text == "Updated group text" } == 1, "Visible group edit was duplicated")

        stage = "member cursors and receipt privacy"
        verify(GroupActions.receipt(recipientEdit, alice.receipts(gid)) == "delivered", "Fetched edit was not delivered")
        bobApi.groupJson(b, "POST", "/v1/groups/$gid/read", buildJsonObject { put("seq", edited.seq) })
        verify(GroupActions.receipt(recipientEdit, alice.receipts(gid)) == "seen", "Read cursor did not produce seen state")
        bobApi.setPolicy(b, "read_receipts", "false")
        verify(alice.receipts(gid).members.all { it.seenSeq == null }, "Recipient opted out but shared seen cursor")
        bobApi.setPolicy(b, "read_receipts", "true"); aliceApi.setPolicy(a, "read_receipts", "false")
        verify(alice.receipts(gid).let { !it.readReceipts && it.members.all { member -> member.seenSeq == null } }, "Sender opt-out did not mask group seen state")
        aliceApi.setPolicy(a, "read_receipts", "true")

        stage = "real committed-send loss, imported state and exact retry"
        alice = GroupClient(crypto, lossyApi, storage, a, sleep = {}, jitter = { 0 }).also { it.init() }
        mode(proxy, "after")
        expectLoss { alice.send(gid, OutgoingDocument("Group accepted before loss")) }
        verify(attempts(proxy).size == 1, "Group POST was replayed without an explicit retry")
        var cmid = storage.rows.getValue(a).pendingSends.keys.single()
        mode(proxy, "off")
        alice = GroupClient(crypto, lossyApi, storage, a, sleep = {}, jitter = { 0 }).also { it.init() }
        verify(attempts(proxy).size == 1, "Restoring an encrypted vault automatically retried")
        val recovered = alice.retry(gid, cmid); sameAttempts(proxy, cmid)
        bob.sync(gid)
        verify(bob.history(gid).count { it.text == "Group accepted before loss" } == 1, "Real retry created a second group message")

        stage = "real committed-edit loss and one recovered revision"
        mode(proxy, "after")
        expectLoss { alice.send(gid, OutgoingDocument("Group edit accepted before loss", editOf = recovered.seq)) }
        cmid = storage.rows.getValue(a).pendingSends.keys.single()
        mode(proxy, "off")
        alice = GroupClient(crypto, lossyApi, storage, a, sleep = {}, jitter = { 0 }).also { it.init() }
        val recoveredEdit = alice.retry(gid, cmid); sameAttempts(proxy, cmid); bob.sync(gid)
        val final = bob.history(gid).single { it.kind == "message" && it.seq == recoveredEdit.seq }
        verify(GroupActions.versions(final, bob.history(gid)).map { it.text } == listOf("Group accepted before loss", "Group edit accepted before loss"), "Edit recovery created extra/missing versions")
        verify(storage.rows.getValue(a).pendingSends.isEmpty(), "Confirmed request remains in outbox")

        stage = "whole-chain deletion and metadata-only offline reconciliation"
        val deletion = WireJson.parseToJsonElement(aliceApi.groupJson(a, "DELETE", "/v1/groups/$gid/messages/${edited.seq}")).jsonObject
        val deleted = deletion["seqs"]!!.jsonArray.map { it.jsonPrimitive.long }
        verify(deleted.containsAll(listOf(root.seq, edited.seq)), "Server deletion did not name every version")
        alice.dropSeqs(gid, deleted, tombstones = true)
        bob.reconcileDeletions(gid)
        verify(bob.history(gid).filter { it.seq in deleted && it.kind == "message" }.all { it.deleted && it.text == null && it.files.isEmpty() }, "Offline reconciliation retained deleted plaintext or attachment keys")
        verify(bob.history(gid).any { it.text == "Group edit accepted before loss" }, "Unrelated chain was erased")
    }
}
