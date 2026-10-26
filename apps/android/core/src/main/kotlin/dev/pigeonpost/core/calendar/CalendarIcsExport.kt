package dev.pigeonpost.core.calendar

import dev.pigeonpost.core.AuthorizationLease
import kotlinx.coroutines.CancellationException
import java.io.IOException
import java.io.OutputStream
import java.util.concurrent.atomic.AtomicBoolean

/** A one-use, in-memory projection. No plaintext staging file survives a cancelled file picker. */
class CalendarIcsExport internal constructor(bytes: ByteArray, authorization: AuthorizationLease) : AutoCloseable {
    private val authorization = authorization.child()
    private val bytes = this.authorization.withActive { require(bytes.size <= 16 * 1024 * 1024); bytes.copyOf() }
    private val consumed = AtomicBoolean(false)

    /** Blocking provider IO: the platform invokes this off its UI thread after the user chooses a file. */
    fun write(open: () -> OutputStream) {
        try {
            authorization.withActive {
                check(consumed.compareAndSet(false, true)) { "This export has already been used. Export the event again." }
                open().use { output ->
                    authorization.requireActive()
                    output.write(bytes)
                    authorization.requireActive()
                    output.flush()
                }
                authorization.requireActive()
            }
        } catch (cancelled: CancellationException) { throw cancelled }
        catch (_: Exception) { throw IOException("The calendar file could not be saved. Export the event again to retry.") }
        finally { close() }
    }
    override fun close() { authorization.retire() }
    override fun toString() = "CalendarIcsExport(bytes=${bytes.size})"
}
