package dev.pigeonpost.core.calendar

import dev.pigeonpost.core.AuthorizationLease
import dev.pigeonpost.core.AuthorizationRetired
import org.junit.Assert.*
import org.junit.Test
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.io.OutputStream

class CalendarIcsExportTest {
    private fun retired(work: () -> Unit) {
        try { work(); fail("A retired export must not write") } catch (_: AuthorizationRetired) { }
    }
    @Test fun inputIsFrozen() {
        val source = byteArrayOf(1, 2, 3)
        val packet = CalendarIcsExport(source, AuthorizationLease())
        source[0] = 9
        val target = ByteArrayOutputStream()
        packet.write { target }
        assertArrayEquals(byteArrayOf(1, 2, 3), target.toByteArray())
    }
    @Test fun aCancelledPickerCannotOpenOrWriteADestinationAndDoesNotRevokeTheLogin() {
        val parent = AuthorizationLease()
        val packet = CalendarIcsExport("private event".toByteArray(), parent)
        packet.close()
        var opened = false
        retired { packet.write { opened = true; ByteArrayOutputStream() } }
        assertFalse(opened); assertTrue(parent.isValid)
    }
    @Test fun parentRetirementPreventsOpeningTheFileAfterAPickerReturns() {
        val parent = AuthorizationLease(); val packet = CalendarIcsExport(byteArrayOf(1), parent)
        parent.retire()
        var opened = false
        retired { packet.write { opened = true; ByteArrayOutputStream() } }
        assertFalse(opened)
    }
    @Test fun retirementDuringProviderOpenClosesItWithoutWritingPrivateBytes() {
        val parent = AuthorizationLease(); val packet = CalendarIcsExport(byteArrayOf(1), parent)
        var closed = false
        val output = object : ByteArrayOutputStream() { override fun close() { closed = true; super.close() } }
        retired { packet.write { parent.retire(); output } }
        assertEquals(0, output.size()); assertTrue(closed)
    }
    @Test fun retirementInsideAnAdmittedWriteCannotPublishSuccess() {
        val parent = AuthorizationLease(); val packet = CalendarIcsExport(byteArrayOf(1), parent)
        var closed = false
        val output = object : OutputStream() {
            override fun write(b: Int) { parent.retire() }
            override fun close() { closed = true }
        }
        retired { packet.write { output } }
        assertTrue(closed)
    }
    @Test fun providerErrorsHaveNoPrivatePathOrNestedCauseAndCannotReuseTheCapsule() {
        val packet = CalendarIcsExport(byteArrayOf(1), AuthorizationLease())
        try { packet.write { throw IOException("private-provider-path") }; fail("Expected a failed save") }
        catch (failure: IOException) { assertFalse(failure.message.orEmpty().contains("private-provider-path")); assertNull(failure.cause) }
        var reopened = false
        retired { packet.write { reopened = true; ByteArrayOutputStream() } }
        assertFalse(reopened)
    }
    @Test fun bytesAndLineEndingsAreExactAndTheCapsuleIsSingleUse() {
        val bytes = "BEGIN:VCALENDAR\r\nSUMMARY:Türkçe\r\nEND:VCALENDAR\r\n".toByteArray()
        val packet = CalendarIcsExport(bytes, AuthorizationLease())
        val output = ByteArrayOutputStream(); packet.write { output }
        assertArrayEquals(bytes, output.toByteArray())
        retired { packet.write { ByteArrayOutputStream() } }
        assertFalse(packet.toString().contains("Türkçe"))
    }
    @Test fun overlargeProjectionsCannotBeRetained() {
        try { CalendarIcsExport(ByteArray(16 * 1024 * 1024 + 1), AuthorizationLease()); fail("Expected a bounded export") }
        catch (_: IllegalArgumentException) { }
    }
}
