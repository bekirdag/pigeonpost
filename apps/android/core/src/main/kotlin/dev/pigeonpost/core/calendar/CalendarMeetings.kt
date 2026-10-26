package dev.pigeonpost.core.calendar

import dev.pigeonpost.core.AuthorizationLease
import dev.pigeonpost.core.AuthorizationRetired
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import java.util.Locale

object CalendarMeetingCode {
    fun parse(input: String): String? {
        if (!Regex("[A-Za-z0-9]{5}-?[A-Za-z0-9]{5}").matches(input)) return null
        val code = input.replace("-", "").uppercase(Locale.ROOT)
        return code.takeIf { it.all { letter -> letter in "ABCDEFGHJKMNPQRSTUVWXYZ23456789" } }
    }
}
interface CalendarMeetingApi {
    suspend fun scheduledMeeting(mailbox: String, code: String): CalendarMeetingStatus
}
@Serializable data class CalendarMeetingStatus(val state: String? = null, val occurrences: List<CalendarMeetingOccurrence>? = null) {
    val choices get() = occurrences.orEmpty().takeIf { state == "open" && it.size <= 50 && it.map { row -> row.occurrenceId }.distinct().size == it.size && it.all(CalendarMeetingOccurrence::valid) }.orEmpty()
}
@Serializable data class CalendarMeetingOccurrence(
    @SerialName("occurrence_id") val occurrenceId: String,
    @SerialName("starts_at") val startsAt: Long? = null,
    @SerialName("ends_at") val endsAt: Long? = null,
    @SerialName("opens_at") val opensAt: Long? = null,
    @SerialName("closes_at") val closesAt: Long? = null,
) {
    val valid: Boolean get() {
        val first = startsAt ?: return false; val last = endsAt ?: return false
        val opens = opensAt ?: return false; val closes = closesAt ?: return false
        return validId(occurrenceId) && opens in 0..253_402_300_799L && closes <= 253_402_300_799L && opens <= first && first < last && last <= closes
    }
    companion object { fun validId(value: String) = Regex("occ_[A-Za-z0-9_]{1,76}").matches(value) }
}

/** A reviewed member join. The call service alone decides roles, admission and the live window. */
class CalendarScheduledJoin(mailbox: String, code: String, val occurrenceId: String, title: String, val video: Boolean, authorization: AuthorizationLease) : AutoCloseable {
    val mailbox = mailbox.also { require(Regex("/k/[A-Za-z0-9_]{1,157}").matches(it)) { "Choose a valid meeting inbox." } }
    val code = requireNotNull(CalendarMeetingCode.parse(code)) { "This meeting code is invalid." }
    val title = title.take(200)
    private val authorization = authorization.child()
    init { require(CalendarMeetingOccurrence.validId(occurrenceId)) { "Choose a specific occurrence." } }
    fun requireCurrent(root: AuthorizationLease?, mailbox: String?) {
        authorization.requireActive()
        if (root == null || !root.isValid || !authorization.sharesAuthorization(root) || mailbox != this.mailbox) throw AuthorizationRetired()
    }
    override fun close() { authorization.retire() }
    override fun toString() = "CalendarScheduledJoin(video=$video)"
}
