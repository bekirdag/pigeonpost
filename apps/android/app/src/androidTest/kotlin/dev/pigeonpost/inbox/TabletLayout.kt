package dev.pigeonpost.inbox

import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.hasTestTag
import androidx.compose.ui.test.junit4.ComposeTestRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.platform.app.InstrumentationRegistry
import dev.pigeonpost.inbox.ui.WideLayoutWidth

/**
 * Whether the app draws its tablet shape (Tablet.kt): the conversations column, the mailbox switcher
 * card and the Calls entry stay beside the open conversation, so there is no "Back to conversations",
 * no inbox picker dialog and no bottom Calls tab. Uses the same threshold as PigeonpostApp.
 */
internal fun wideLayout(): Boolean =
    InstrumentationRegistry.getInstrumentation().targetContext.resources.configuration.screenWidthDp >= WideLayoutWidth.value

/** The Calls entry: a bottom tab on a phone, a sidebar row on a tablet. Both carry the missed-call description. */
internal val callsEntry: SemanticsMatcher = hasTestTag("tab:calls") or hasTestTag("sidebar:calls")

/** A row of the tablet's inbox switcher, by display name (rows are tagged with the inbox's canonical key, not its handle). */
internal fun mailboxRow(name: String): SemanticsMatcher = hasText(name) and SemanticsMatcher("tag starts with mailbox:") { node ->
    node.config.getOrElseNullable(SemanticsProperties.TestTag) { null }?.startsWith("mailbox:") == true
}

/** Opens the list of this account's inboxes: a dialog from the "Inbox" title on a phone, the switcher card on a tablet. */
internal fun ComposeTestRule.openMailboxes() {
    if (wideLayout()) onNodeWithTag("mailboxSwitcher").performClick() else onNodeWithText("Inbox").performClick()
}
