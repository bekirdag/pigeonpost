package dev.pigeonpost.core.calendar

/** Untrusted FCM locators only. No title, token, organizer or role grants member authority. */
class CalendarNotice private constructor(val mailbox: String, val event: CalendarEventKey?) {
    override fun toString() = "CalendarNotice(hasEvent=${event != null})"
    companion object {
        fun matches(data: Map<String, String>) = data["type"] == "calendar" || data["message_id"]?.startsWith("cal:") == true
        private fun ascii(value: String) = Regex("[A-Za-z0-9_]+").matches(value)
        private fun locator(value: String?, prefix: String) = value != null && value.startsWith(prefix) && value.length in (prefix.length + 1)..80 && ascii(value)
        fun parse(data: Map<String, String>): CalendarNotice? {
            if (!matches(data)) return null
            // FCM's `mailbox` is a display subtitle. Only `identity` is the canonical recipient.
            val mailbox = data["identity"] ?: return null
            if (!mailbox.startsWith("/k/") || mailbox.length !in 4..160 || !ascii(mailbox.drop(3))) return null
            val event = data["calendar_event_id"]
            val occurrence = data["calendar_occurrence_id"]
            val key = if (locator(event, "evt_") && (occurrence == null || locator(occurrence, "occ_"))) CalendarEventKey(event!!, occurrence) else null
            // A malformed or legacy locator opens Calendar, never guesses an event from message_id.
            return CalendarNotice(mailbox, key)
        }
    }
}
