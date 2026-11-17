package dev.pigeonpost.inbox.widgets

import androidx.compose.ui.unit.DpSize
import androidx.compose.ui.unit.dp
import androidx.glance.appwidget.testing.unit.runGlanceAppWidgetUnitTest
import androidx.glance.testing.unit.hasContentDescription
import androidx.glance.testing.unit.hasContentDescriptionEqualTo
import androidx.glance.testing.unit.hasText
import androidx.glance.testing.unit.hasTextEqualTo
import dev.pigeonpost.core.widgets.WidgetNests
import dev.pigeonpost.core.widgets.WidgetSnapshot
import dev.pigeonpost.core.widgets.WidgetSnapshots
import org.junit.After
import org.junit.Before
import org.junit.Test
import java.time.ZoneOffset
import java.util.Locale
import java.util.TimeZone

/**
 * The widget layouts at the sizes a launcher gives them (2x1 to tablet), in each state: signed
 * out, locked, empty, and with content. Message text must never appear unless previews are on.
 */
class WidgetLayoutTest {
    private val now = java.time.LocalDateTime.of(2026, 11, 15, 9, 0).toEpochSecond(ZoneOffset.UTC)
    private val sample = WidgetSamples.snapshot(now, ZoneOffset.UTC)
    private lateinit var locale: Locale
    private lateinit var zone: TimeZone

    @Before fun pin() { locale = Locale.getDefault(); zone = TimeZone.getDefault(); Locale.setDefault(Locale.UK); TimeZone.setDefault(TimeZone.getTimeZone("UTC")) }
    @After fun restore() { Locale.setDefault(locale); TimeZone.setDefault(zone) }

    private fun inbox(size: DpSize, snapshot: WidgetSnapshot?, check: androidx.glance.appwidget.testing.unit.GlanceAppWidgetUnitTest.() -> Unit) =
        runGlanceAppWidgetUnitTest { setAppWidgetSize(size); provideComposable { InboxContent(snapshot, now) }; check() }

    // ---- inbox ------------------------------------------------------------------------------------

    @Test fun inboxStripShowsTheCountOnly() = inbox(InboxWidget.STRIP, sample) {
        onNode(hasTextEqualTo("8")).assertExists()
        onNode(hasTextEqualTo("unread")).assertExists()
        onNode(hasText("Maya Okafor")).assertDoesNotExist()
        onNode(hasContentDescriptionEqualTo("New message")).assertDoesNotExist()
    }

    @Test fun wideStripAddsTheLatestUnreadConversationAndNewMessage() = inbox(InboxWidget.WIDE_STRIP, sample) {
        onNode(hasTextEqualTo("8")).assertExists()
        onNode(hasTextEqualTo("Maya Okafor")).assertExists()
        onNode(hasContentDescriptionEqualTo("New message")).assertExists()
    }

    @Test fun squareListsNamesWithoutSecondLines() = inbox(InboxWidget.SQUARE, sample) {
        onNode(hasTextEqualTo("Inbox")).assertExists()
        onNode(hasTextEqualTo("Maya Okafor")).assertExists()
        onNode(hasTextEqualTo("2 new messages")).assertDoesNotExist()
    }

    @Test fun mediumAndLargeShowNewMessageInsteadOfText() {
        for (size in listOf(InboxWidget.MEDIUM, InboxWidget.LARGE, InboxWidget.TABLET)) inbox(size, sample) {
            onNode(hasTextEqualTo("Inbox")).assertExists()
            onNode(hasTextEqualTo("Maya Okafor")).assertExists()
            onNode(hasTextEqualTo("2 new messages")).assertExists()
            onNode(hasTextEqualTo("Design review")).assertExists()
            onNode(hasTextEqualTo("5 new messages")).assertExists()
            onNode(hasTextEqualTo("New message")).assertExists()
            onNode(hasContentDescriptionEqualTo("New message")).assertExists()
        }
    }

    @Test fun previewsAppearOnlyWhenTheSnapshotCarriesThem() {
        val hidden = sample.copy(conversations = sample.conversations.mapIndexed { i, row -> if (i == 0) row.copy(preview = "See you at six") else row })
        // A snapshot that says previews are off loses any text on the way in, as the app reads it.
        val read = WidgetSnapshots.decode(WidgetSnapshots.encode(hidden))
        inbox(InboxWidget.LARGE, read) { onNode(hasText("See you at six")).assertDoesNotExist() }
        inbox(InboxWidget.LARGE, WidgetSnapshots.decode(WidgetSnapshots.encode(hidden.copy(previews = true)))) {
            onNode(hasTextEqualTo("See you at six")).assertExists()
        }
    }

    @Test fun signedOutAsksToSignIn() {
        for (size in InboxWidget.SIZES) inbox(size, null) {
            onNode(hasTextEqualTo("Sign in to Pigeonpost")).assertExists()
            onNode(hasText("Maya")).assertDoesNotExist()
        }
    }

    @Test fun lockedShowsCountsOnly() = inbox(InboxWidget.LARGE, WidgetSnapshots.redact(sample)) {
        onNode(hasTextEqualTo("8 new messages")).assertExists()
        onNode(hasText("Maya")).assertDoesNotExist()
        onNode(hasText("Design review")).assertDoesNotExist()
    }

    @Test fun emptyInboxSaysSo() = inbox(InboxWidget.MEDIUM, WidgetSamples.empty(now)) {
        onNode(hasTextEqualTo("No conversations yet")).assertExists()
    }

    @Test fun rowsDescribeThemselvesForTalkBack() = inbox(InboxWidget.LARGE, sample) {
        onNode(hasContentDescriptionEqualTo("Maya Okafor, 2 new messages")).assertExists()
        onNode(hasContentDescriptionEqualTo("Jonas Berg, /jonas")).assertExists()
    }

    // ---- quick actions ----------------------------------------------------------------------------

    @Test fun oneByOneIsNewMessage() = runGlanceAppWidgetUnitTest {
        setAppWidgetSize(QuickActionsWidget.ONE); provideComposable { QuickActionsContent(sample) }
        onNode(hasContentDescriptionEqualTo("New message")).assertExists()
        onNode(hasTextEqualTo("Inbox")).assertDoesNotExist()
    }

    @Test fun twoByOneAddsInboxWithItsCount() = runGlanceAppWidgetUnitTest {
        setAppWidgetSize(QuickActionsWidget.TWO); provideComposable { QuickActionsContent(sample) }
        onNode(hasTextEqualTo("New")).assertExists()
        onNode(hasTextEqualTo("Inbox")).assertExists()
        onNode(hasContentDescriptionEqualTo("Open inbox, 8 unread")).assertExists()
    }

    // ---- nests ------------------------------------------------------------------------------------

    @Test fun nestsShowStatusAndHours() = runGlanceAppWidgetUnitTest {
        setAppWidgetSize(NestsWidget.MEDIUM); provideComposable { NestsContent(sample) }
        onNode(hasTextEqualTo("Research helper")).assertExists()
        onNode(hasTextEqualTo("Ready")).assertExists()
        onNode(hasTextEqualTo("46 h of 120 h")).assertExists()
        onNode(hasTextEqualTo("Paused")).assertExists()
    }

    @Test fun smallNestsDropTheMeter() = runGlanceAppWidgetUnitTest {
        setAppWidgetSize(NestsWidget.SQUARE); provideComposable { NestsContent(sample) }
        onNode(hasTextEqualTo("Research helper")).assertExists()
        onNode(hasTextEqualTo("46 h of 120 h")).assertDoesNotExist()
    }

    @Test fun noNestsOffersOne() = runGlanceAppWidgetUnitTest {
        setAppWidgetSize(NestsWidget.MEDIUM); provideComposable { NestsContent(WidgetSamples.empty(now)) }
        onNode(hasTextEqualTo("No Nests yet")).assertExists()
        onNode(hasTextEqualTo("Get a Nest")).assertExists()
    }

    @Test fun nestsOffForTheAccount() = runGlanceAppWidgetUnitTest {
        setAppWidgetSize(NestsWidget.MEDIUM); provideComposable { NestsContent(sample.copy(nests = WidgetNests(enabled = false))) }
        onNode(hasTextEqualTo("Nests are not available")).assertExists()
        onNode(hasText("Get a Nest")).assertDoesNotExist()
    }

    @Test fun lockedNestsAreCounted() = runGlanceAppWidgetUnitTest {
        setAppWidgetSize(NestsWidget.LARGE); provideComposable { NestsContent(WidgetSnapshots.redact(sample)) }
        onNode(hasTextEqualTo("3 Nests")).assertExists()
        onNode(hasText("Research helper")).assertDoesNotExist()
    }

    // ---- agenda -----------------------------------------------------------------------------------

    @Test fun agendaGroupsByDay() {
        for (size in listOf(AgendaWidget.MEDIUM, AgendaWidget.LARGE, AgendaWidget.TABLET)) runGlanceAppWidgetUnitTest {
            setAppWidgetSize(size); provideComposable { AgendaContent(sample, now) }
            onNode(hasTextEqualTo("Sunday 15 November")).assertExists()
            onNode(hasTextEqualTo("TODAY")).assertExists()
            onNode(hasTextEqualTo("Design review")).assertExists()
            onNode(hasTextEqualTo("15:30 – 16:15")).assertExists()
            onNode(hasTextEqualTo("19:00 – 21:00 · Harbour Kitchen")).assertExists()
            onNode(hasTextEqualTo("TOMORROW")).assertExists()
            onNode(hasTextEqualTo("All day")).assertExists()
            onAllNodes(hasContentDescription("Pigeonpost meeting")).assertCountEquals(2)
        }
    }

    @Test fun smallAgendaKeepsTitlesAndTimes() = runGlanceAppWidgetUnitTest {
        setAppWidgetSize(AgendaWidget.SQUARE); provideComposable { AgendaContent(sample, now) }
        onNode(hasTextEqualTo("Sun 15")).assertExists()
        onNode(hasTextEqualTo("Design review")).assertExists()
        onNode(hasTextEqualTo("19:00 – 21:00")).assertExists()
    }

    @Test fun pastEventsDropOffBetweenSnapshots() = runGlanceAppWidgetUnitTest {
        val evening = now + 11 * 3600 // 20:00, after the design review
        setAppWidgetSize(AgendaWidget.LARGE); provideComposable { AgendaContent(sample, evening) }
        onNode(hasText("Design review")).assertDoesNotExist()
        onNode(hasTextEqualTo("Dinner with Maya")).assertExists()
    }

    @Test fun emptyAgendaSaysNothingIsScheduled() = runGlanceAppWidgetUnitTest {
        setAppWidgetSize(AgendaWidget.MEDIUM); provideComposable { AgendaContent(WidgetSamples.empty(now), now) }
        onNode(hasTextEqualTo("Nothing scheduled")).assertExists()
    }

    @Test fun calendarNotLoadedYet() = runGlanceAppWidgetUnitTest {
        setAppWidgetSize(AgendaWidget.MEDIUM); provideComposable { AgendaContent(sample.copy(agenda = null), now) }
        onNode(hasTextEqualTo("Open your calendar")).assertExists()
    }

    @Test fun lockedAgendaIsCounted() = runGlanceAppWidgetUnitTest {
        setAppWidgetSize(AgendaWidget.LARGE); provideComposable { AgendaContent(WidgetSnapshots.redact(sample), now) }
        onNode(hasTextEqualTo("5 upcoming events")).assertExists()
        onNode(hasText("Dinner")).assertDoesNotExist()
    }

    // ---- shared rules -----------------------------------------------------------------------------

    @Test fun subtitleNeverInventsText() {
        val row = sample.conversations.first()
        org.junit.Assert.assertEquals("2 new messages", subtitle(row))
        org.junit.Assert.assertEquals("New message", subtitle(row.copy(unread = 1)))
        org.junit.Assert.assertEquals("/maya", subtitle(row.copy(unread = 0)))
        org.junit.Assert.assertEquals("No new messages", subtitle(row.copy(peer = "/k/abc", unread = 0)))
        org.junit.Assert.assertEquals("Group", subtitle(row.copy(group = true, unread = 0)))
        org.junit.Assert.assertEquals("hi", subtitle(row.copy(preview = "hi")))
    }

    @Test fun nestHoursUseOnlyWhatIsKnown() {
        val nest = sample.nests!!.nests.first()
        org.junit.Assert.assertEquals("46 h of 120 h", nestHours(nest, 120)!!.first)
        org.junit.Assert.assertEquals("46 h this month", nestHours(nest, null)!!.first)
        org.junit.Assert.assertEquals("120 h a month", nestHours(nest.copy(runningSecs = null), 120)!!.first)
        org.junit.Assert.assertNull(nestHours(nest.copy(runningSecs = null), null))
    }
}
