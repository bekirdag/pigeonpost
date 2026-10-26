package dev.pigeonpost.core.calendar

import dev.pigeonpost.core.AuthorizationLease
import dev.pigeonpost.core.validAddress
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonPrimitive
import java.io.IOException

@Serializable enum class CalendarAttemptKind {
    @SerialName("event") EVENT,
    @SerialName("calendar") CALENDAR,
    @SerialName("import") IMPORT,
}

/** Contains private authored data. Persist only through protected mailbox-scoped storage. */
@Serializable data class CalendarPendingAttempt(
    val id: String,
    val mailbox: String,
    val kind: CalendarAttemptKind,
    val encoded: String,
    @SerialName("created_at") val createdAt: Long,
    val target: String? = null,
) {
    fun payload() = CalendarWritePayload.restore(encoded)
    fun canRetry(now: Long): Boolean {
        val age = try { Math.subtractExact(now, createdAt) } catch (_: ArithmeticException) { return false }
        return age in 0..86400
    }
    override fun toString() = "CalendarPendingAttempt(kind=$kind, createdAt=$createdAt)"
}
@Serializable data class CalendarAttemptEnvelope(val version: Int = 1, val attempts: List<CalendarPendingAttempt> = emptyList())

class CalendarAttemptStorageException : IOException("Saved calendar changes could not be opened. Try again before creating another event.")

/** Missing is empty; corrupt/unreadable must throw, never masquerade as no pending creation. */
interface CalendarAttemptStorage {
    val persistent: Boolean get() = true
    suspend fun load(mailbox: String, authorization: AuthorizationLease): List<CalendarPendingAttempt>
    suspend fun save(mailbox: String, attempts: List<CalendarPendingAttempt>, authorization: AuthorizationLease)
}

object CalendarAttempts {
    const val MAX_PAYLOAD_BYTES = 512 * 1024
    const val MAX_FILE_BYTES = 4 * 1024 * 1024
    private val identifier = Regex("[A-Za-z0-9_-]{1,128}")
    fun validate(mailbox: String, attempts: List<CalendarPendingAttempt>) {
        require(validAddress(mailbox))
        require(attempts.size <= 3 && attempts.distinctBy { it.kind }.size == attempts.size)
        require(attempts.distinctBy { it.id }.size == attempts.size)
        for (attempt in attempts) {
            require(attempt.mailbox == mailbox && identifier.matches(attempt.id) && attempt.createdAt >= 0)
            require(attempt.encoded.toByteArray(Charsets.UTF_8).size <= MAX_PAYLOAD_BYTES)
            val fields = attempt.payload().fields()
            when (attempt.kind) {
                CalendarAttemptKind.EVENT, CalendarAttemptKind.CALENDAR -> {
                    val key = fields["request_key"] as? JsonPrimitive
                    require(attempt.target == null && key?.isString == true && key.content == attempt.id && key.content.length <= 64)
                }
                CalendarAttemptKind.IMPORT -> require(attempt.target?.let(identifier::matches) == true)
            }
        }
    }
}

/** Debug/host-test storage only. The live app supplies an Android Keystore implementation. */
class MemoryCalendarAttemptStorage : CalendarAttemptStorage {
    override val persistent = false
    private val lock = Mutex()
    private val rows = mutableMapOf<String, List<CalendarPendingAttempt>>()
    override suspend fun load(mailbox: String, authorization: AuthorizationLease): List<CalendarPendingAttempt> {
        authorization.requireActive()
        return lock.withLock { authorization.withActive { rows[mailbox].orEmpty().also { CalendarAttempts.validate(mailbox, it) }.toList() } }
    }
    override suspend fun save(mailbox: String, attempts: List<CalendarPendingAttempt>, authorization: AuthorizationLease) {
        val frozen = attempts.toList()
        authorization.requireActive()
        lock.withLock {
            authorization.withActive {
                CalendarAttempts.validate(mailbox, frozen)
                if (frozen.isEmpty()) rows.remove(mailbox) else rows[mailbox] = frozen
            }
        }
    }
}
