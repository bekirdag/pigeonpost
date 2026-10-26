package dev.pigeonpost.core.calendar

import dev.pigeonpost.core.AuthorizationLease
import dev.pigeonpost.core.AuthorizationRetired
import dev.pigeonpost.core.WireJson
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class CalendarAttemptsTest {
    private fun attempt(id: String = "fixture-key", mailbox: String = "/alice", at: Long = 100) = CalendarPendingAttempt(id, mailbox, CalendarAttemptKind.EVENT,
        CalendarWritePayload(buildJsonObject { put("request_key", id); put("title", "Private fixture event"); put("description", "Exact bytes: İğdır ✓"); putJsonArray("attendees") { add(buildJsonObject { put("contact", "guest@fixture.invalid") }) } }).text, at)
    private suspend inline fun <reified T : Throwable> fails(noinline work: suspend () -> Unit) {
        assertTrue(runCatching { work() }.exceptionOrNull() is T)
    }
    @Test fun anUnconfirmedPayloadKeepsItsIdentityAndBytesAcrossRecordRestoration() {
        val first = attempt()
        val encoded = WireJson.encodeToString(CalendarAttemptEnvelope.serializer(), CalendarAttemptEnvelope(attempts = listOf(first)))
        val restored = WireJson.decodeFromString<CalendarAttemptEnvelope>(encoded).attempts.single()
        CalendarAttempts.validate("/alice", listOf(restored))
        assertEquals(first.id, restored.id); assertEquals(first.createdAt, restored.createdAt)
        assertArrayEquals(first.payload().bytes(), restored.payload().bytes())
    }
    @Test fun retiringAnActorCannotReadOverwriteOrEraseItsReplacementAttempt() = runBlocking<Unit> {
        val storage = MemoryCalendarAttemptStorage(); val old = AuthorizationLease(); val current = AuthorizationLease()
        storage.save("/alice", listOf(attempt("old-key")), old)
        old.retire()
        val replacement = attempt("current-key")
        storage.save("/alice", listOf(replacement), current)
        fails<AuthorizationRetired> { storage.load("/alice", old) }
        fails<AuthorizationRetired> { storage.save("/alice", listOf(attempt("old-key")), old) }
        fails<AuthorizationRetired> { storage.save("/alice", emptyList(), old) }
        assertEquals(listOf(replacement), storage.load("/alice", current))
    }
    @Test fun anAttemptCannotBeMovedIntoAnotherMailboxOrGivenANewKey() = runBlocking<Unit> {
        val storage = MemoryCalendarAttemptStorage(); val lease = AuthorizationLease()
        fails<IllegalArgumentException> { storage.save("/bob", listOf(attempt()), lease) }
        fails<IllegalArgumentException> { storage.save("/alice", listOf(attempt().copy(id = "changed-key")), lease) }
        assertTrue(storage.load("/bob", lease).isEmpty())
    }
    @Test fun oneUnknownWritePerKindAndPayloadBoundsPreventSilentReplacement() = runBlocking<Unit> {
        fails<IllegalArgumentException> { CalendarAttempts.validate("/alice", listOf(attempt("one"), attempt("two"))) }
        val huge = CalendarWritePayload(buildJsonObject { put("request_key", "fixture-key"); put("title", "x".repeat(CalendarAttempts.MAX_PAYLOAD_BYTES)) })
        fails<IllegalArgumentException> { CalendarAttempts.validate("/alice", listOf(attempt().copy(encoded = huge.text))) }
        fails<IllegalArgumentException> { CalendarAttempts.validate("/alice", listOf(attempt().copy(target = "another-target"))) }
        val imported = CalendarPendingAttempt("import-key", "/alice", CalendarAttemptKind.IMPORT, CalendarWritePayload(buildJsonObject { putJsonArray("select") { add(JsonPrimitive(0)) } }).text, 100, "imp_one")
        CalendarAttempts.validate("/alice", listOf(attempt(), imported))
        fails<IllegalArgumentException> { CalendarAttempts.validate("/alice", listOf(imported.copy(target = "../other"))) }
    }
    @Test fun retryAgeNeverGetsResetByClockChangesOrIntegerOverflow() {
        val pending = attempt(at = 100)
        assertTrue(pending.canRetry(100)); assertTrue(pending.canRetry(86500))
        assertFalse(pending.canRetry(99)); assertFalse(pending.canRetry(86501))
        assertFalse(pending.canRetry(Long.MIN_VALUE)); assertFalse(pending.canRetry(Long.MAX_VALUE))
        assertFalse(attempt(at = Long.MAX_VALUE).canRetry(Long.MIN_VALUE))
    }
    @Test fun nonCanonicalOrUnknownAttemptRecordsAreNotSilentlyRewritten() = runBlocking<Unit> {
        fails<IllegalArgumentException> { CalendarAttempts.validate("/alice", listOf(attempt().copy(encoded = "{ \"request_key\":\"fixture-key\" }"))) }
        val encoded = WireJson.encodeToString(CalendarAttemptEnvelope.serializer(), CalendarAttemptEnvelope(attempts = listOf(attempt())))
        fails<kotlinx.serialization.SerializationException> { WireJson.decodeFromString<CalendarAttemptEnvelope>(encoded.replace("\"kind\":\"event\"", "\"kind\":\"future-unknown\"")) }
    }
    @Test fun callersCannotMutateAStoredAttemptListAfterSaving() = runBlocking<Unit> {
        val storage = MemoryCalendarAttemptStorage(); val lease = AuthorizationLease()
        val input = mutableListOf(attempt())
        storage.save("/alice", input, lease); input.clear()
        assertEquals(1, storage.load("/alice", lease).size)
        storage.save("/alice", emptyList(), lease); assertTrue(storage.load("/alice", lease).isEmpty())
    }
    @Test fun debugDescriptionsDoNotContainAuthoredContentOrContactAddresses() {
        val value = attempt().toString()
        assertFalse(value.contains("Private fixture")); assertFalse(value.contains("guest@fixture.invalid")); assertFalse(value.contains("request_key"))
    }
}
