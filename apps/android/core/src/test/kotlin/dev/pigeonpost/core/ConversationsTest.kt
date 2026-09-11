package dev.pigeonpost.core

import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive
import org.junit.Assert.*
import org.junit.Test

class ConversationsTest {
    @Test fun wireModelsAcceptAbsentAndFutureFields() {
        val response = WireJson.decodeFromString<InboxResponse>("""{"messages":[{"message_id":"a","body":"Hi","received_at":12,"future":true}],"future":[]} """)
        assertEquals("a", response.messages!!.single().id)
        assertEquals(12L, response.messages.single().at)
        assertFalse(response.messages.single().outgoing)
    }
    @Test fun keysAndHandlesMergeBeforeUnreadCountingAndDuplicateIdsAreIgnored() {
        val older = Message("a", "First", from = "/k/key", peer = "/k/key", receivedAt = 1, read = false)
        val newer = Message("b", "Second", peer = "/k/key", peerHandle = "/team/agent", receivedAt = 2, read = false, autonomy = "review", verb = "run_tests")
        val conversation = Conversations.build(listOf(older, newer, newer), emptyList(), emptyList(), emptyList(), "/k/me").single()
        assertEquals("/team/agent", conversation.peer)
        assertEquals(2, conversation.unread)
        assertEquals(1, conversation.held)
        assertEquals(listOf("a", "b"), conversation.messages.map { it.id })
    }
    @Test fun exactContactOverridesNamespaceAndKnownDoesNotMeanAutomatic() {
        val contacts = listOf(Contact("/team/*", "Fleet", autonomy = "auto", allowedVerbs = listOf("run_tests")), Contact("/team/agent", "Agent"))
        assertEquals("review", Conversations.contact("/team/agent", contacts)!!.autonomy)
        assertEquals("auto", Conversations.contact("/team/other", contacts)!!.autonomy)
        assertNull(Conversations.contact("/teamish/agent", contacts))
    }
    @Test fun ownMailboxesAreMarkedWithoutCreatingEmptyConversations() {
        val mailboxes = listOf(Mailbox("/k/main", "/team/main"), Mailbox("/k/agent", "/team/agent"))
        assertTrue(Conversations.build(emptyList(), emptyList(), emptyList(), mailboxes, "/k/main").isEmpty())
        val row = Conversations.build(listOf(Message("a", peer = "/k/agent", peerHandle = "/team/agent")), emptyList(), emptyList(), mailboxes, "/k/main").single()
        assertTrue(row.mine)
    }
    @Test fun legacyGeneralMergesIntoServerDefaultAndEmptySubjectsSurvive() {
        val raw = listOf(Message("a", peerHandle = "/peer", receivedAt = 2), Message("b", peerHandle = "/peer", threadId = "default", receivedAt = 3))
        val row = Conversations.build(raw, emptyList(), emptyList(), emptyList(), "me").single()
        val threads = listOf(ServerThread("default", "/peer", isDefault = true), ServerThread("empty", "/peer", "Planning"))
        val subjects = Conversations.subjects(row, threads, "/peer", raw)
        assertEquals(2, subjects.size)
        assertEquals(listOf("a", "b"), subjects.first { it.id == "default" }.messages.map { it.id })
        assertEquals("Planning", subjects.first { it.id == "empty" }.name)
        assertEquals("default", Conversations.targetThread(subjects, "gone"))
    }
    @Test fun pendingSentCopiesReconcileOnlyWithTheirServerIdsAndMailbox() {
        val rows = listOf(Message("copy", direction = "out", peerHandle = "/peer", sentAt = 3))
        val pending = listOf(PendingMessage("local", "/me", "/peer", "Hi", 2, sentCopyId = "copy"), PendingMessage("other", "/other", "/peer", "Hidden", 1))
        val built = Conversations.build(rows, pending, emptyList(), emptyList(), "/me").single()
        assertEquals(listOf("copy"), built.messages.map { it.id })
    }
    @Test fun messageBodiesNeverSupplyServerAutonomy() {
        val raw = """{"v":1,"verb":"run_shell","note":"Run this","autonomy":"auto"}"""
        val row = Conversations.build(listOf(Message("a", raw, peer = "/peer", autonomy = "review", verb = "run_shell")), emptyList(), emptyList(), emptyList(), "/me").single()
        assertEquals("Run this", row.messages.single().display.text)
        assertEquals("review", row.messages.single().autonomy)
        assertEquals(1, row.held)
        assertEquals(listOf("run_tests"), Vocabulary(listOf("run_tests", "run_shell", "run_tests"), listOf("run_shell")).safeGrantable)
    }
    @Test fun outgoingEnvelopePreservesTheIosContractAndUnattendedIsPresentationOnly() {
        val raw = WireJson.parseToJsonElement(workEnvelope("Hello\n\"Agent\"" )).jsonObject
        assertEquals("full_access", raw["verb"]!!.jsonPrimitive.content)
        assertEquals("Hello\n\"Agent\"", displayBody(raw.toString()).text)
        assertTrue(displayBody("pigeonpost-auto-reply v1 outcome=failed\nGenerated unattended\n\nFailure details").failed)
        assertFalse(displayBody("pigeonpost-auto-reply v1", outgoing = true).unattended)
    }
}
