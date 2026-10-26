package dev.pigeonpost.core

import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonArray
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray
import okhttp3.HttpUrl.Companion.toHttpUrl
import java.io.File
import java.io.IOException
import java.net.URI
import java.net.http.HttpClient
import java.net.http.HttpRequest
import java.net.http.HttpResponse
import kotlin.system.exitProcess

/** Opt-in interoperability executable. All accounts, bodies and faults belong to the local fixture. */
object MessageActionsPostbox {
    private var stage = "setup"
    private val control = HttpClient.newBuilder().followRedirects(HttpClient.Redirect.NEVER).build()

    @JvmStatic fun main(args: Array<String>) {
        try {
            require(args.size == 1)
            runBlocking { qualify(WireJson.parseToJsonElement(File(args.single()).readText()).jsonObject) }
            println("PASS: Android message actions, receipt privacy, references and committed-loss retry.")
            exitProcess(0)
        } catch (failure: Throwable) {
            // Never print the fixture JSON, token, HTTP headers or parser input on a setup failure.
            System.err.println("FAIL: message-action interop at $stage (${failure.javaClass.simpleName}).")
            if (failure is AssertionError) System.err.println(failure.message)
            exitProcess(1)
        }
    }

    private data class Person(val address: String, val token: String, val api: PostboxClient)
    private fun origin(value: String): String {
        val url = value.toHttpUrl()
        require(url.scheme == "http" && url.host == "127.0.0.1" && url.port in 1..65535)
        require(url.username.isEmpty() && url.password.isEmpty() && url.encodedPath == "/" && url.query == null && url.fragment == null)
        return value.trimEnd('/')
    }
    private fun client(base: String, token: String) = PostboxClient(object : TokenProvider {
        override suspend fun token(rejected: String?) = token
    }, (base + "/").toHttpUrl(), allowLoopbackForTests = true)
    private fun verify(value: Boolean, label: String) { if (!value) throw AssertionError(label) }
    private suspend fun rejects(label: String, block: suspend () -> Unit) {
        try { block() } catch (failure: ApiException) {
            verify(failure.status in setOf(403, 404, 409), "$label: expected a reference/ownership refusal")
            return
        }
        throw AssertionError("$label: request was accepted")
    }
    private fun request(base: String, path: String, body: JsonObject? = null, token: String? = null): JsonObject {
        val builder = HttpRequest.newBuilder(URI.create(base + path))
        token?.let { builder.header("authorization", "Bearer $it") }
        if (body != null) builder.header("content-type", "application/json").POST(HttpRequest.BodyPublishers.ofString(body.toString()))
        val response = control.send(builder.build(), HttpResponse.BodyHandlers.ofString())
        verify(response.statusCode() in 200..299, "Fixture control request failed (${response.statusCode()})")
        return WireJson.parseToJsonElement(response.body()).jsonObject
    }
    private fun mode(proxy: String, next: String) { request(proxy, "/__test/control", buildJsonObject { put("mode", next) }) }
    private fun attempts(proxy: String) = request(proxy, "/__test/attempts")["attempts"]!!.jsonArray.map { it.jsonObject }
    private suspend fun loss(label: String, block: suspend () -> Unit) {
        try { block() } catch (failure: IOException) {
            verify(failure !is ApiException, "$label: must lose transport after a successful server commit")
            return
        }
        throw AssertionError("$label: response unexpectedly arrived")
    }

    private suspend fun qualify(setup: JsonObject) {
        val base = origin(setup["postbox"]!!.jsonPrimitive.content)
        val proxy = origin(setup["lossProxy"]!!.jsonPrimitive.content)
        val people = setup["people"]!!.jsonObject
        fun person(name: String): Person {
            val data = people[name]!!.jsonObject
            val credential = data["token"]!!.jsonPrimitive.content
            return Person(data["address"]!!.jsonPrimitive.content, credential, client(base, credential))
        }
        val alice = person("alice")
        val bob = person("bob")
        val carol = person("carol")
        stage = "capabilities and explicit local policies"
        verify(alice.api.capabilities().enabled.containsAll(Features.MESSAGE_ACTIONS), "Real postbox did not enable all action capabilities")
        for (person in listOf(alice, bob, carol)) {
            person.api.setPolicy(person.address, "read_receipts", "true")
            person.api.setPolicy(person.address, "unnamed_messages", "show")
            person.api.setPolicy(person.address, "unknown_messages", "show")
            person.api.setPolicy(person.address, "low_reputation", "allow")
        }
        alice.api.saveContact(alice.address, Contact(bob.address, alias = "Bob", autonomy = "review"))
        bob.api.saveContact(bob.address, Contact(alice.address, alias = "Alice", autonomy = "review"))
        suspend fun sent(ref: String) = alice.api.inboxWithActions(alice.address).messages.orEmpty().single { it.outgoing && it.msgRef == ref }
        suspend fun received(ref: String) = bob.api.inboxWithActions(bob.address).messages.orEmpty().single { !it.outgoing && it.msgRef == ref }

        stage = "copy references, delivery and seen receipt"
        val original = MessageSendAction()
        val initial = alice.api.sendWithActions(alice.address, bob.address, "Original interview time", null, original)
        verify(initial.msgRef == original.msgRef && initial.messageId != null && initial.sentCopyId != null,
            "Send response must identify the request and both copies")
        verify(initial.messageId != initial.sentCopyId, "Sender and receiver need distinct own-copy IDs")
        val senderCopy = requireNotNull(initial.sentCopyId)
        val incoming = received(original.msgRef)
        verify(incoming.id == initial.messageId && incoming.body == "Original interview time", "Recipient copy did not decode")
        verify(sent(original.msgRef).receipt?.state == "delivered", "Fetching the inbox must record delivery")
        bob.api.ackWithEdits(bob.address, incoming.id)
        verify(sent(original.msgRef).receipt?.state == "seen", "Explicit acknowledgement must record seen")
        request(base, "/v1/receipts", buildJsonObject {
            put("identity", bob.address); put("kind", "agent_status"); put("status", "working")
            putJsonArray("message_ids") { add(JsonPrimitive(incoming.id)) }
        }, bob.token)
        verify(sent(original.msgRef).agentStatus?.state == "working", "Agent status must decode independently of message text")

        stage = "folded edits, fresh unread revision and version history"
        val edit = MessageSendAction(editOf = initial.sentCopyId)
        val edited = alice.api.sendWithActions(alice.address, bob.address, "Updated interview time", null, edit)
        verify(edited.msgRef == edit.msgRef, "Edit request needs its own stable retry reference")
        val folded = received(original.msgRef)
        verify(folded.id == incoming.id && folded.body == "Updated interview time" && folded.editCount == 1,
            "Folded edit must replace the original body without adding a bubble")
        verify(folded.read != true, "A previously acknowledged message's new revision must be unread")
        val versions = alice.api.messageInfo(alice.address, initial.sentCopyId!!).versions
        verify(versions.map { it.body } == listOf("Original interview time", "Updated interview time"), "Info must preserve original and updated text")
        bob.api.ackWithEdits(bob.address, folded.id)
        verify(received(original.msgRef).read == true, "Folded acknowledgement must include the edit revision")

        stage = "reply context and mailbox isolation"
        val reply = MessageSendAction(replyTo = incoming.id, quote = "Updated interview time")
        val answer = bob.api.sendWithActions(bob.address, alice.address, "That time works", null, reply)
        val answerIn = alice.api.inboxWithActions(alice.address).messages.orEmpty().single { it.id == answer.messageId }
        verify(answerIn.replyTo?.msgRef == original.msgRef && answerIn.replyTo?.messageId == initial.sentCopyId,
            "Reply parent must resolve to the viewer's own copy")
        verify(answerIn.replyTo?.quote == "Updated interview time", "Reply quote must survive encryption and JSON decoding")
        rejects("Cross-pair reply") {
            alice.api.sendWithActions(alice.address, carol.address, "Invalid cross-pair reply", null, MessageSendAction(replyTo = initial.sentCopyId))
        }
        rejects("Editing someone else's message") {
            bob.api.sendWithActions(bob.address, alice.address, "Invalid edit", null, MessageSendAction(editOf = incoming.id))
        }
        rejects("Another account's message info") { carol.api.messageInfo(carol.address, senderCopy) }

        stage = "recipient read-receipt preference"
        bob.api.setPolicy(bob.address, "read_receipts", "false")
        val privateAction = MessageSendAction()
        alice.api.sendWithActions(alice.address, bob.address, "Private reading", null, privateAction)
        val privateIncoming = received(privateAction.msgRef)
        bob.api.ackWithEdits(bob.address, privateIncoming.id)
        verify(sent(privateAction.msgRef).receipt?.let { it.state == "delivered" && it.seenAt == null } == true,
            "Recipient preference must suppress seen receipts")
        bob.api.setPolicy(bob.address, "read_receipts", "true")
        alice.api.setPolicy(alice.address, "read_receipts", "false")
        verify(sent(original.msgRef).receipt?.let { it.state == "delivered" && it.seenAt == null } == true,
            "Sender preference must also mask historical seen state")
        alice.api.setPolicy(alice.address, "read_receipts", "true")

        stage = "filtered-message receipt privacy"
        bob.api.setPolicy(bob.address, "unknown_messages", "filter")
        val strangerAction = MessageSendAction()
        val stranger = carol.api.sendWithActions(carol.address, bob.address, "Unknown sender", null, strangerAction)
        verify(bob.api.inboxWithActions(bob.address).messages.orEmpty().none { it.msgRef == strangerAction.msgRef }, "Filtered message leaked into normal inbox")
        verify(bob.api.filteredInboxWithActions(bob.address).single { it.msgRef == strangerAction.msgRef }.id == stranger.messageId,
            "Filtered actions view must decode the recipient copy")
        bob.api.ackWithEdits(bob.address, stranger.messageId!!)
        request(base, "/v1/receipts", buildJsonObject {
            put("identity", bob.address); put("kind", "agent_status"); put("status", "working")
            putJsonArray("message_ids") { add(JsonPrimitive(stranger.messageId)) }
        }, bob.token)
        val strangerSent = carol.api.inboxWithActions(carol.address).messages.orEmpty().single { it.outgoing && it.msgRef == strangerAction.msgRef }
        verify(strangerSent.receipt?.state != "seen" && strangerSent.receipt?.seenAt == null && strangerSent.agentStatus == null,
            "Filtered mail must never share seen or agent activity")

        stage = "lost committed message and exact explicit retry"
        val lossy = client(proxy, alice.token)
        val action = MessageSendAction()
        mode(proxy, "after")
        loss("Committed send") { lossy.sendWithActions(alice.address, bob.address, "Saved before transport loss", null, action) }
        verify(attempts(proxy).size == 1, "Client must not automatically replay a POST")
        mode(proxy, "off")
        val recovered = lossy.sendWithActions(alice.address, bob.address, "Saved before transport loss", null, action)
        verify(recovered.duplicate == true && recovered.msgRef == action.msgRef, "Explicit retry must recover the accepted request")
        val sendAttempts = attempts(proxy)
        verify(sendAttempts.size == 2 && sendAttempts.map { it["sha"] }.distinct().size == 1 && sendAttempts.all { it["msgRef"]?.jsonPrimitive?.content == action.msgRef },
            "Retry must retain byte-identical body and request reference")
        verify(sendAttempts.all { it["status"]!!.jsonPrimitive.int in 200..299 }, "Transport loss must follow a successful upstream response")
        verify(received(action.msgRef).id == recovered.messageId, "Explicit retry created an extra recipient copy")

        stage = "lost committed edit and exactly one version"
        val lostEdit = MessageSendAction(editOf = recovered.sentCopyId)
        mode(proxy, "after")
        loss("Committed edit") { lossy.sendWithActions(alice.address, bob.address, "Edited before transport loss", null, lostEdit) }
        mode(proxy, "off")
        val recoveredEdit = lossy.sendWithActions(alice.address, bob.address, "Edited before transport loss", null, lostEdit)
        verify(recoveredEdit.duplicate == true, "Edit retry must recover the accepted version")
        verify(attempts(proxy).let { it.size == 2 && it.map { a -> a["sha"] }.distinct().size == 1 }, "Edit retry body changed")
        verify(alice.api.messageInfo(alice.address, recovered.sentCopyId!!).versions.map { it.body } ==
            listOf("Saved before transport loss", "Edited before transport loss"), "Edit retry must create exactly one revision")
        verify(received(action.msgRef).body == "Edited before transport loss", "Folded recipient lost the recovered edit")
    }
}
