package dev.pigeonpost.core.groups

import dev.pigeonpost.core.WireJson
import org.junit.Assert.*
import org.junit.Test

class GroupActionsTest {
    private val original = HistoryItem(kind = "message", seq = 1L, sender = "alice", text = "Original", createdAt = 100L)
    private val edit = HistoryItem(kind = "message", seq = 2L, sender = "alice", text = "Changed", createdAt = 110L, editOf = "1", actionsVerified = true)

    @Test fun onlyCanonicalPositiveSequenceReferencesAreAccepted() {
        for (value in listOf(null, "0", "-1", "01", "+1", "1.0", "9223372036854775808", " 1")) assertNull(value, GroupActions.sequence(value))
        assertEquals(12L, GroupActions.sequence("12"))
    }
    @Test fun serverHintsAndUnverifiedOrForeignEditsCannotHideAMessage() {
        val hinted = original.copy(replacedBy = 2L)
        for (replacement in listOf(edit.copy(actionsVerified = false), edit.copy(sender = "mallory"), edit.copy(editOf = "02"))) {
            assertEquals(listOf("Original", "Changed"), GroupActions.visible(listOf(hinted, replacement), 120L).map { it.text })
        }
        assertEquals(listOf("Changed"), GroupActions.visible(listOf(hinted, edit), 120L).map { it.text })
    }
    @Test fun versionsFollowOnlyEarlierSameAuthorLinks() {
        val latest = edit.copy(seq = 3L, editOf = "2", text = "Latest")
        val history = listOf(original, edit, latest)
        assertEquals(listOf("Original", "Changed", "Latest"), GroupActions.versions(latest, history).map { it.text })
        assertEquals(listOf("Latest"), GroupActions.visible(history, 130L).map { it.text })
        assertNull(GroupActions.parent(edit.copy(editOf = "2"), history))
        assertNull(GroupActions.parent(edit.copy(editOf = "3"), history))
    }
    @Test fun missingOriginalAndLegacyEditsCannotAcquireANewEditWindow() {
        assertFalse(GroupActions.editable(edit, listOf(edit), "alice", 120L))
        assertFalse(GroupActions.editable(edit.copy(actionsVerified = false), listOf(original, edit.copy(actionsVerified = false)), "alice", 120L))
        assertTrue(GroupActions.editable(edit, listOf(original, edit), "alice", 120L))
        assertFalse(GroupActions.editable(edit, listOf(original, edit), "alice", 86_500L))
        assertFalse(GroupActions.editable(edit, listOf(original, edit), "bob", 120L))
    }
    @Test fun anEditCannotExtendTheOriginalDisappearanceDeadline() {
        val root = original.copy(expiresAt = 200L)
        val latest = edit.copy(expiresAt = 500L)
        val history = listOf(root, latest)
        assertEquals(listOf("Changed"), GroupActions.visible(history, 199L).map { it.text })
        assertTrue(GroupActions.visible(history, 200L).isEmpty())
        assertTrue(GroupActions.pruning(history, 200L).isEmpty())
    }
    @Test fun deletingOneVerifiedVersionErasesPlaintextKeysAndDependentPendingEdits() {
        val file = GroupFile("file", "private.txt", "text/plain", 6L, key = "secret", sha = "digest")
        val latest = edit.copy(files = listOf(file))
        val pending = latest.copy(seq = null, cmid = "pending", editOf = "2", pending = true)
        val unrelated = original.copy(seq = 7L, sender = "mallory", text = "Other", editOf = "1", actionsVerified = true)
        val result = GroupActions.erasing(listOf(original, latest, pending, unrelated), setOf(2L))
        assertEquals(setOf(1L, 2L, 7L), result.mapNotNull { it.seq }.toSet())
        assertTrue(result.filter { it.seq in setOf(1L, 2L) }.all { it.deleted && it.text == null && it.files.isEmpty() && it.editOf == null && it.cmid == null })
        assertEquals("Other", result.single { it.seq == 7L }.text)
    }
    @Test fun receiptAggregationRequiresAllDeliveredAndAtLeastOneVisibleSeenCursor() {
        val bob = GroupMemberReceipt("bob", deliveredSeq = 2L, seenSeq = 2L)
        val carol = GroupMemberReceipt("carol", deliveredSeq = 2L, seenSeq = null)
        assertEquals("sent", GroupActions.receipt(edit, GroupReceipts()))
        assertEquals("sent", GroupActions.receipt(edit, GroupReceipts(listOf(bob, carol.copy(deliveredSeq = 1L)), true)))
        assertEquals("seen", GroupActions.receipt(edit, GroupReceipts(listOf(bob, carol), true)))
        assertEquals("delivered", GroupActions.receipt(edit, GroupReceipts(listOf(bob.copy(seenSeq = 1L), carol), true)))
        assertEquals("delivered", GroupActions.receipt(edit, GroupReceipts(listOf(bob, carol), false)))
        assertEquals("delivered", GroupActions.receipt(edit, GroupReceipts(listOf(bob.copy(seenSeq = null), carol), true)))
    }
    @Test fun oldVaultsHaveNoAuthenticatedActionProofOrPendingRequests() {
        val record = WireJson.decodeFromString<GroupRecord>("""{"device_id":"device","state_key":"state","history":{"grp":[{"kind":"message","seq":1,"edit_of":"2","replaced_by":3}]}}""")
        assertTrue(record.pendingSends.isEmpty())
        assertFalse(record.history.getValue("grp").single().actionsVerified)
        assertEquals(record, WireJson.decodeFromString<GroupRecord>(WireJson.encodeToString(GroupRecord.serializer(), record)))
    }
}
