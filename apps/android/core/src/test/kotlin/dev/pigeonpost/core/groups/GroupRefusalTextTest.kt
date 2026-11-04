package dev.pigeonpost.core.groups

import dev.pigeonpost.core.ApiException
import dev.pigeonpost.core.MessageActionKind
import dev.pigeonpost.core.explainMessageActionRefusal
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/** A refused group or 1:1 edit or reply is worded for what was refused, never as "groups are off" (§14C). */
class GroupRefusalTextTest {

    @Test
    fun groupsOffStillSaysGroupsOff() {
        assertEquals("Groups are not enabled for this account.",
            explainGroupError(ApiException(404, "feature_disabled"), GroupComposerKind.EDIT))
    }

    @Test
    fun editOffIsNotGroupsOff() {
        assertEquals("Editing messages is not turned on for this account.",
            explainGroupError(ApiException(403, "feature_disabled"), GroupComposerKind.EDIT))
    }

    @Test
    fun replyOffIsNotGroupsOff() {
        assertEquals("Replying to a message is not turned on for this account.",
            explainGroupError(ApiException(403, "feature_disabled"), GroupComposerKind.REPLY))
    }

    @Test
    fun editWindow() {
        assertTrue(explainGroupError(ApiException(403, "not_editable"), GroupComposerKind.EDIT)
            .startsWith("This message can no longer be edited."))
    }

    @Test
    fun otherRefusals() {
        assertEquals("Only admins can send messages here.", explainGroupError(ApiException(403, "posting_restricted")))
        assertEquals("This organization is read-only until its payment is settled.", explainGroupError(ApiException(403, "org_read_only")))
        assertEquals("This group is out of storage.", explainGroupError(ApiException(409, "quota_exceeded")))
        assertEquals("That mailbox is banned from this group.", explainGroupError(ApiException(403, "banned")))
        assertTrue(explainGroupError(ApiException(403, "unknown_device")).startsWith("This device is no longer in the group."))
        assertEquals("Too many requests. Wait a moment and try again.", explainGroupError(ApiException(429, null)))
    }

    @Test
    fun refusedSendIsNotUnconfirmed() {
        assertEquals("Editing messages is not turned on for this account.",
            explainGroupSendFailure(ApiException(403, "feature_disabled"), GroupComposerKind.EDIT))
        assertTrue(explainGroupSendFailure(ApiException(503, "unavailable")).startsWith("Could not confirm the send: "))
    }

    @Test
    fun dmRefusals() {
        assertEquals("Replying to a message is not turned on for this account.",
            explainMessageActionRefusal(ApiException(403, "feature_disabled"), MessageActionKind.REPLY))
        assertTrue(explainMessageActionRefusal(ApiException(403, "edit_window_closed"), MessageActionKind.EDIT)!!
            .startsWith("This message can no longer be edited."))
        assertNull(explainMessageActionRefusal(ApiException(404, "feature_disabled"), MessageActionKind.EDIT))
        assertEquals("An edit changes the text only. Send files in a new message.",
            explainMessageActionRefusal(ApiException(400, "edit_has_attachments"), MessageActionKind.EDIT))
        assertNull(explainMessageActionRefusal(ApiException(403, "feature_disabled"), MessageActionKind.SEND))
    }
}
