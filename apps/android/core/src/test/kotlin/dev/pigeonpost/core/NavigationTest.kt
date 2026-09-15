package dev.pigeonpost.core

import org.junit.Assert.*
import org.junit.Test

class NavigationTest {
    private fun box(handle: String?, id: String = handle.orEmpty()) = Mailbox(id, handle)

    @Test fun mailboxCategoriesAreStableAndPrimaryIsAlwaysFirst() {
        val rows = listOf(box(null, "/k/z"), box("/bekir/agent"), box("/studio/main"), box("/alp"), box("/alp/worker"), box("/bekir/main"), box("/k/hash"))
        assertEquals(listOf("/bekir/main", "/alp", "/studio/main", "/alp/worker", "/bekir/agent", "/k/hash", null), orderedMailboxes(rows, " /BEKIR/ ").map { it.handle })
        assertEquals(orderedMailboxes(rows, "bekir"), orderedMailboxes(rows.toList(), "bekir"))
    }

    @Test fun missingUsernameFallsBackToFirstOwnedRootAndKeepsTiesStable() {
        val first = box("/studio/main", "first")
        val twin = box("/ALP", "second")
        val rows = listOf(box("/studio/child"), first, twin, box("/alp", "third"))
        assertEquals(listOf("first", "second", "third", "/studio/child"), orderedMailboxes(rows, null).map { it.address })
        assertEquals(orderedMailboxes(rows, null), orderedMailboxes(rows, "unowned"))
        assertTrue(orderedMailboxes(emptyList(), null).isEmpty())
    }

    @Test fun historyStartsWithLatestTenAndPagesToTheOldestWithoutDuplicates() {
        val ids = (0 until 1000).map { it.toString() }
        var window = HistoryWindow().latest(ids)
        assertEquals((990..999).reversed().map { it.toString() }, window.ids)
        repeat(99) { window = window.older(ids) }
        assertEquals(ids.reversed(), window.ids)
        assertEquals(window, window.older(ids))
        assertEquals(1000, window.ids.toSet().size)
    }

    @Test fun arrivalsAndDeletionDoNotReplaceTheReadingWindow() {
        val ids = (0 until 45).map { it.toString() }
        val reading = HistoryWindow().latest(ids).older(ids)
        val updated = reading.update(ids + "arrived" + "other-device-outgoing", followingLatest = false)
        assertEquals(reading.ids, updated.ids)
        assertEquals(reading.ids - "40", updated.update(ids - "40", false).ids)
        assertEquals("arrived", reading.update(ids + "arrived", true).ids.first())
        assertEquals(10, reading.update(ids + "arrived", true).ids.size)
    }

    @Test fun searchRevealsAnOldMatchWithContextAndLatestResetsTheWindow() {
        val ids = (0 until 1000).map { it.toString() }
        val window = HistoryWindow().latest(ids)
        val found = window.reveal(ids, "123")
        assertTrue("123" in found.ids)
        assertEquals("114", found.ids.last())
        assertEquals(window, found.latest(ids))
        assertEquals(window, window.reveal(ids, "missing"))
    }

    @Test fun emptyAndShortHistoriesStayBounded() {
        val window = HistoryWindow()
        assertTrue(window.latest(emptyList()).ids.isEmpty())
        assertEquals(listOf("only"), window.latest(listOf("only")).ids)
        assertEquals(listOf("only"), window.update(listOf("only"), false).ids)
    }
}
