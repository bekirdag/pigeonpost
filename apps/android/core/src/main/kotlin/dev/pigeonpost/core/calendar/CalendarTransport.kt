package dev.pigeonpost.core.calendar

/** Member calendar routes only. Guest invitation/verification credentials stay in the web flow. */
interface CalendarTransport {
    suspend fun calendarRequest(
        mailbox: String,
        method: String,
        path: String,
        query: Map<String, String> = emptyMap(),
        body: ByteArray? = null,
        mediaType: String = "application/json",
    ): ByteArray
}
