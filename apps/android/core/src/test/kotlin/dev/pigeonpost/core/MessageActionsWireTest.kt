package dev.pigeonpost.core

import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import okhttp3.mockwebserver.SocketPolicy
import org.junit.After
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import java.io.IOException

class MessageActionsWireTest {
    private lateinit var server: MockWebServer
    private lateinit var client: PostboxClient
    private val ref = "mref_abcdefghijklmnopqrstuvwxyz"

    @Before fun setup() {
        server = MockWebServer().also { it.start(java.net.InetAddress.getByName("127.0.0.1"), 0) }
        client = PostboxClient(object : TokenProvider {
            override suspend fun token(rejected: String?) = "fixture-session"
        }, server.url("/").newBuilder().host("127.0.0.1").build(), allowLoopbackForTests = true)
    }
    @After fun teardown() { server.shutdown() }

    @Test fun foldedReadsCarryMetadataAndPreserveLegacyOptOut() = runBlocking {
        server.enqueue(MockResponse().setBody("""{"messages":[{"message_id":"root","body":"changed","direction":"out","msg_ref":"$ref","edited_at":120,"edit_count":2,"reply_to":{"msg_ref":"parent","from":"/peer/main","excerpt":"Earlier","quote":"Fallback"},"receipt":{"state":"seen","delivered_at":101,"seen_at":110},"agent_status":{"state":"done","at":115}}]}"""))
        val message = client.inboxWithActions("/team/main", 25).messages!!.single()
        val request = server.takeRequest()
        assertEquals("/team/main", request.requestUrl!!.queryParameter("identity"))
        assertEquals("1", request.requestUrl!!.queryParameter("actions"))
        assertEquals("1", request.requestUrl!!.queryParameter("fold"))
        assertEquals("25", request.requestUrl!!.queryParameter("wait"))
        assertEquals(ref, message.msgRef)
        assertEquals(120L, message.editedAt)
        assertEquals(2, message.editCount)
        assertEquals("/peer/main", message.replyTo!!.sender)
        assertEquals("Fallback", message.replyTo!!.quote)
        assertEquals(110L, message.receipt!!.seenAt)
        assertEquals("done", message.agentStatus!!.state)
        server.enqueue(MockResponse().setBody("""{"messages":[{"message_id":"old","body":"old text"}]}"""))
        val legacy = client.inbox("/team/main").messages!!.single()
        val oldRequest = server.takeRequest()
        assertNull(oldRequest.requestUrl!!.queryParameter("actions"))
        assertNull(oldRequest.requestUrl!!.queryParameter("fold"))
        assertNull(legacy.msgRef)
        assertNull(legacy.receipt)
    }

    @Test fun aLostSendResponseNeedsExplicitRetryWithTheSameReferenceAndBody() = runBlocking {
        server.enqueue(MockResponse().setSocketPolicy(SocketPolicy.DISCONNECT_AFTER_REQUEST))
        val action = MessageSendAction(replyTo = "own_parent_copy", quote = "Parent text")
        try {
            withTimeout(5_000) { client.sendWithActions("/me/main", "/peer/main", "Answer", "subject", action) }
            fail("Expected an unconfirmed send")
        } catch (_: IOException) {}
        assertEquals(1, server.requestCount)
        server.enqueue(MockResponse().setBody("""{"message_id":"received","sent_copy_id":"sent","msg_ref":"${action.msgRef}","duplicate":true}"""))
        val recovered = client.sendWithActions("/me/main", "/peer/main", "Answer", "subject", action)
        val attempts = List(2) { server.takeRequest() }
        val first = attempts[0].body.readUtf8()
        assertEquals(first, attempts[1].body.readUtf8())
        val body = WireJson.parseToJsonElement(first).jsonObject
        assertEquals(action.msgRef, body["msg_ref"]!!.jsonPrimitive.content)
        assertEquals("own_parent_copy", body["reply_to"]!!.jsonPrimitive.content)
        assertEquals("Parent text", body["quote"]!!.jsonPrimitive.content)
        assertFalse(body.containsKey("edit_of"))
        assertEquals(true, recovered.duplicate)
        assertEquals("sent", recovered.sentCopyId)
        assertEquals(2, server.requestCount)
    }

    @Test fun editsKeepTheOriginalReferenceAndRejectAttachmentsBeforeSending() = runBlocking {
        val action = MessageSendAction(editOf = "own_original_copy")
        assertThrows(IllegalArgumentException::class.java) {
            runBlocking { client.sendWithActions("/me", "/peer", "Changed", null, action, listOf("file")) }
        }
        assertEquals(0, server.requestCount)
        server.enqueue(MockResponse().setBody("""{"sent_copy_id":"edit"}"""))
        client.sendWithActions("/me", "/peer", "Changed", null, action)
        val body = WireJson.parseToJsonElement(server.takeRequest().body.readUtf8()).jsonObject
        assertEquals("own_original_copy", body["edit_of"]!!.jsonPrimitive.content)
        assertFalse(body.containsKey("reply_to"))
        assertFalse(body.containsKey("attachments"))
    }

    @Test fun foldedAcknowledgmentsUseABooleanAndLegacyAcksStayUnchanged() = runBlocking {
        repeat(2) { server.enqueue(MockResponse().setBody("{}")) }
        client.ackWithEdits("/me", "root")
        val folded = WireJson.parseToJsonElement(server.takeRequest().body.readUtf8()).jsonObject
        assertEquals("/me", folded["identity"]!!.jsonPrimitive.content)
        assertEquals("true", folded["with_edits"].toString())
        client.ack("/me", "old")
        val legacy = WireJson.parseToJsonElement(server.takeRequest().body.readUtf8()).jsonObject
        assertFalse(legacy.containsKey("with_edits"))
    }

    @Test fun messageInfoDecodesVersionIdsAndKeepsTheRequestedMailbox() = runBlocking {
        server.enqueue(MockResponse().setBody("""{"message_id":"root","msg_ref":"$ref","direction":"out","sent_at":90,"versions":[{"message_id":"root","body":"First","at":90},{"message_id":"edit","body":"Second","at":100}],"reply_to":{"quote":"Parent no longer present"},"agent_status":{"state":"working"}}"""))
        val info = client.messageInfo("/me/main", "root")
        val request = server.takeRequest()
        assertEquals("/v1/messages/root/info", request.requestUrl!!.encodedPath)
        assertEquals("/me/main", request.requestUrl!!.queryParameter("identity"))
        assertEquals(listOf("root", "edit"), info.versions.map { it.id })
        assertEquals("Second", info.versions.last().body)
        assertNull(info.replyTo!!.sender)
        assertNull(info.agentStatus!!.at)
        assertThrows(IllegalArgumentException::class.java) {
            runBlocking { client.messageInfo("/me", "../victim") }
        }
        assertEquals(1, server.requestCount)
    }

    @Test fun readReceiptSettingsSendBooleansWithoutOverwritingOtherSettings() = runBlocking {
        for (value in listOf(false, true)) {
            server.enqueue(MockResponse().setBody("""{"read_receipts":$value,"unknown_calls":"silence"}"""))
            val policy = client.setPolicy("/me", "read_receipts", value.toString())
            val request = server.takeRequest()
            assertEquals("PUT", request.method)
            val body = WireJson.parseToJsonElement(request.body.readUtf8()).jsonObject
            assertEquals(setOf("identity", "read_receipts"), body.keys)
            assertEquals(value.toString(), body["read_receipts"].toString())
            assertEquals(value, policy.readReceipts)
            assertEquals("silence", policy.unknownCalls)
        }
        assertEquals(false, FilterPolicy().with("read_receipts", "false").readReceipts)
    }

    @Test fun invalidActionsNeverBecomeAnOrdinarySendAndQuotesCountUnicodeCharacters() {
        assertThrows(IllegalArgumentException::class.java) { MessageSendAction(msgRef = "bad") }
        assertThrows(IllegalArgumentException::class.java) { MessageSendAction(replyTo = "root", editOf = "other") }
        assertThrows(IllegalArgumentException::class.java) { MessageSendAction(quote = "No parent") }
        assertThrows(IllegalArgumentException::class.java) { MessageSendAction(replyTo = "../other") }
        val full = "🙂".repeat(200)
        assertEquals(full, MessageSendAction(replyTo = "parent", quote = full).quote)
        assertThrows(IllegalArgumentException::class.java) { MessageSendAction(replyTo = "parent", quote = full + "x") }
    }
}
