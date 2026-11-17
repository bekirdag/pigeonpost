package dev.pigeonpost.core.widgets

import dev.pigeonpost.core.displayName
import dev.pigeonpost.core.validAddress
import dev.pigeonpost.core.groups.GroupLinks
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.Json
import kotlinx.serialization.json.int
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.jsonPrimitive

/**
 * What the home-screen widgets may show (docs/widgets-plan-android.md §3).
 *
 * The app writes this snapshot; the widgets only read it. It never holds a token, a key, crypto
 * state or anything a widget could use to reach the postbox. It is versioned: a widget that meets a
 * snapshot of another version shows nothing rather than guess. Message text is present only when
 * the person switched "Show message previews in widgets" on; group messages never carry text,
 * because they are only ever decrypted inside the app.
 */
@Serializable
data class WidgetSnapshot(
    val version: Int = WidgetSnapshots.VERSION,
    /** The acting mailbox: every tap target and every push update must name this mailbox. */
    val mailbox: String,
    /** Epoch seconds. */
    @SerialName("generated_at") val generatedAt: Long,
    /** The app is locked: counts only, no names, no titles, no previews. */
    val locked: Boolean = false,
    val previews: Boolean = false,
    /** Unread messages across direct conversations and groups. */
    val unread: Int = 0,
    val conversations: List<WidgetConversation> = emptyList(),
    val nests: WidgetNests? = null,
    val agenda: WidgetAgenda? = null,
    /** Push message ids already counted, so a repeated push is not counted twice. */
    @SerialName("push_ids") val pushIds: List<String> = emptyList(),
) {
    override fun toString() = "WidgetSnapshot(v$version, ${conversations.size} conversations, redacted)"
}

@Serializable
data class WidgetConversation(
    /** The peer address, or a group id (`grp_…`): what a tap opens. */
    val peer: String,
    val name: String,
    /** Epoch seconds of the latest message. */
    val at: Long = 0,
    val unread: Int = 0,
    /** The latest message's first line; null unless previews are on. */
    val preview: String? = null,
    val group: Boolean = false,
) {
    override fun toString() = "WidgetConversation(redacted)"
}

@Serializable
data class WidgetNests(
    /** The account has Nests at all (the `nests` feature). */
    val enabled: Boolean,
    val nests: List<WidgetNest> = emptyList(),
    /** How many Nests there are; kept when [nests] is redacted. */
    val count: Int = nests.size,
    /** The package's pooled running hours a month, when known. */
    @SerialName("hours_limit") val hoursLimit: Int? = null,
)

@Serializable
data class WidgetNest(
    val id: String,
    val label: String,
    /** The postbox's status word: ready, working, needs_you, paused, unavailable. */
    val status: String,
    @SerialName("status_reason") val statusReason: String? = null,
    /** Running time this period, from the last usage the app read; null when never read. */
    @SerialName("running_secs") val runningSecs: Long? = null,
)

@Serializable
data class WidgetAgenda(
    val enabled: Boolean,
    /** The calendar's display zone id. */
    val zone: String,
    val events: List<WidgetEvent> = emptyList(),
    /** How many events are listed; kept when [events] is redacted. */
    val count: Int = events.size,
)

@Serializable
data class WidgetEvent(
    @SerialName("event_id") val eventId: String,
    @SerialName("occurrence_id") val occurrenceId: String,
    val title: String,
    /** Epoch seconds; null for an all-day event. */
    @SerialName("starts_at") val startsAt: Long? = null,
    @SerialName("ends_at") val endsAt: Long? = null,
    @SerialName("all_day") val allDay: Boolean = false,
    /** ISO dates of an all-day event; the end is exclusive. */
    @SerialName("start_date") val startDate: String? = null,
    @SerialName("end_date") val endDate: String? = null,
    val location: String? = null,
    /** The event has a Pigeonpost meeting attached. */
    val meeting: Boolean = false,
)

/** One conversation or group as the app knows it, before the widget rules are applied. */
data class WidgetSourceConversation(val peer: String, val name: String, val at: Long, val unread: Int, val lastText: String? = null, val group: Boolean = false)

data class WidgetSourceNests(val nests: List<WidgetNest>, val hoursLimit: Int? = null)

data class WidgetSourceEvent(
    val eventId: String, val occurrenceId: String, val title: String?, val startsAt: Long?, val endsAt: Long?, val allDay: Boolean,
    val startDate: String?, val endDate: String?, val location: String?, val status: String?, val myRsvp: String?, val meeting: Boolean,
)

data class WidgetSourceAgenda(val zone: String, val events: List<WidgetSourceEvent>)

/**
 * Everything the app passes in. A null section means "not loaded in this session": the previous
 * snapshot's section is kept, so a widget does not go blank while the app is still loading.
 */
data class WidgetInput(
    val mailbox: String,
    val conversations: List<WidgetSourceConversation>,
    val showPreviews: Boolean,
    val locked: Boolean = false,
    /** null: not loaded yet. Ignored when [nestsEnabled] is false. */
    val nests: WidgetSourceNests? = null,
    val nestsEnabled: Boolean = false,
    /** null: not loaded yet, or the calendar shows another month. Ignored when [calendarEnabled] is false. */
    val agenda: WidgetSourceAgenda? = null,
    val calendarEnabled: Boolean = false,
)

object WidgetSnapshots {
    const val VERSION = 1
    const val MAX_CONVERSATIONS = 12
    const val MAX_NESTS = 8
    const val MAX_EVENTS = 12
    const val AGENDA_DAYS = 14L
    const val PREVIEW_CHARS = 120
    const val NAME_CHARS = 64
    const val TITLE_CHARS = 80
    private const val PUSH_IDS = 32

    private val json = Json { ignoreUnknownKeys = true; explicitNulls = false; encodeDefaults = true }

    fun encode(snapshot: WidgetSnapshot): String = json.encodeToString(WidgetSnapshot.serializer(), snapshot)

    /** A snapshot of this version, or null: unreadable, another version, or no valid mailbox. */
    fun decode(text: String?): WidgetSnapshot? {
        if (text.isNullOrBlank()) return null
        return try {
            val version = json.parseToJsonElement(text).jsonObject["version"]?.jsonPrimitive?.int
            if (version != VERSION) return null
            json.decodeFromString(WidgetSnapshot.serializer(), text).takeIf { validAddress(it.mailbox) }?.let(::sanitized)
        } catch (_: Exception) { null }
    }

    /** One line of plain text: no control characters, collapsed whitespace, at most [max] characters. */
    fun clean(text: String?, max: Int): String? {
        val flat = text?.filterNot { it.isISOControl() && it != '\n' && it != '\t' }?.split(Regex("\\s+"))?.filter { it.isNotEmpty() }?.joinToString(" ")
        if (flat.isNullOrEmpty()) return null
        return if (flat.length <= max) flat else flat.take(max - 1).trimEnd() + "…"
    }

    /** The first non-empty line of a message, with Markdown heading and quote marks dropped. */
    fun preview(body: String?): String? {
        val line = body?.lineSequence()?.map { it.trim().trimStart('#', '>', ' ') }?.firstOrNull { it.isNotBlank() }
        return clean(line, PREVIEW_CHARS)
    }

    fun build(input: WidgetInput, previous: WidgetSnapshot?, now: Long): WidgetSnapshot {
        require(validAddress(input.mailbox)) { "The acting mailbox is not a Pigeonpost address." }
        val sameMailbox = previous?.mailbox == input.mailbox && previous.version == VERSION
        val rows = input.conversations
            .filter { (it.group && GroupLinks.isGroupId(it.peer)) || (!it.group && validAddress(it.peer)) }
            .sortedWith(compareByDescending<WidgetSourceConversation> { it.at }.thenBy { it.peer })
        val conversations = rows.take(MAX_CONVERSATIONS).map { row ->
            WidgetConversation(
                peer = row.peer,
                name = clean(row.name, NAME_CHARS) ?: if (row.group) "Group" else displayName(row.peer),
                at = row.at.coerceAtLeast(0),
                unread = row.unread.coerceAtLeast(0),
                // Group text is never decrypted outside the app; a direct message only with consent.
                preview = if (input.showPreviews && !row.group) preview(row.lastText) else null,
                group = row.group,
            )
        }
        val nests = when {
            !input.nestsEnabled -> WidgetNests(enabled = false)
            input.nests != null -> input.nests.let { source ->
                val known = previous?.nests?.nests.orEmpty().takeIf { sameMailbox }.orEmpty().associateBy { it.id }
                val list = source.nests.map { nest ->
                    nest.copy(label = clean(nest.label, NAME_CHARS) ?: "Nest", status = clean(nest.status, 24) ?: "unavailable",
                        statusReason = clean(nest.statusReason, 32),
                        runningSecs = (nest.runningSecs ?: known[nest.id]?.runningSecs)?.coerceAtLeast(0))
                }
                WidgetNests(enabled = true, nests = list.take(MAX_NESTS), count = list.size, hoursLimit = source.hoursLimit?.takeIf { it > 0 })
            }
            sameMailbox && previous?.nests?.enabled == true -> previous.nests
            else -> null
        }
        val agenda = when {
            !input.calendarEnabled -> WidgetAgenda(enabled = false, zone = input.agenda?.zone ?: "UTC")
            input.agenda != null -> agenda(input.agenda, now)
            sameMailbox && previous?.agenda?.enabled == true -> previous.agenda
            else -> null
        }
        val built = WidgetSnapshot(
            mailbox = input.mailbox, generatedAt = now, locked = input.locked, previews = input.showPreviews && !input.locked,
            unread = rows.sumOf { it.unread.coerceAtLeast(0) }, conversations = conversations, nests = nests, agenda = agenda,
            pushIds = if (sameMailbox) previous!!.pushIds else emptyList(),
        )
        return if (input.locked) redact(built) else built
    }

    private fun agenda(source: WidgetSourceAgenda, now: Long): WidgetAgenda {
        val zone = runCatching { java.time.ZoneId.of(source.zone) }.getOrDefault(java.time.ZoneOffset.UTC)
        val today = java.time.Instant.ofEpochSecond(now).atZone(zone).toLocalDate()
        val horizon = today.plusDays(AGENDA_DAYS)
        val horizonSeconds = horizon.atStartOfDay(zone).toEpochSecond()
        val events = source.events.filter { it.status != "cancelled" && it.myRsvp != "declined" }.filter { event ->
            if (event.allDay) {
                val start = event.startDate?.let { runCatching { java.time.LocalDate.parse(it) }.getOrNull() } ?: return@filter false
                val end = event.endDate?.let { runCatching { java.time.LocalDate.parse(it) }.getOrNull() } ?: start.plusDays(1)
                end > today && start < horizon
            } else {
                val start = event.startsAt ?: return@filter false
                (event.endsAt ?: start) >= now && start < horizonSeconds
            }
        }.sortedWith(compareBy<WidgetSourceEvent> { sortKey(it, zone) }.thenBy { it.eventId }.thenBy { it.occurrenceId })
        val list = events.map { event ->
            WidgetEvent(eventId = event.eventId, occurrenceId = event.occurrenceId, title = clean(event.title, TITLE_CHARS) ?: "Untitled event",
                startsAt = event.startsAt.takeUnless { event.allDay }, endsAt = event.endsAt.takeUnless { event.allDay }, allDay = event.allDay,
                startDate = event.startDate.takeIf { event.allDay }, endDate = event.endDate.takeIf { event.allDay },
                location = clean(event.location, TITLE_CHARS), meeting = event.meeting)
        }
        return WidgetAgenda(enabled = true, zone = zone.id, events = list.take(MAX_EVENTS), count = list.size)
    }

    private fun sortKey(event: WidgetSourceEvent, zone: java.time.ZoneId): Long =
        if (event.allDay) event.startDate?.let { runCatching { java.time.LocalDate.parse(it).atStartOfDay(zone).toEpochSecond() }.getOrNull() } ?: Long.MAX_VALUE
        else event.startsAt ?: Long.MAX_VALUE

    /** Counts only: no peer, name, preview, Nest label or event title survives. */
    fun redact(snapshot: WidgetSnapshot): WidgetSnapshot = snapshot.copy(
        locked = true, previews = false, conversations = emptyList(),
        nests = snapshot.nests?.let { it.copy(nests = emptyList(), count = it.count) },
        agenda = snapshot.agenda?.let { it.copy(events = emptyList(), count = it.count) },
    )

    /** A decoded snapshot obeys the same limits a built one does, whatever was written to disk. */
    private fun sanitized(snapshot: WidgetSnapshot): WidgetSnapshot {
        val clean = snapshot.copy(
            unread = snapshot.unread.coerceAtLeast(0),
            conversations = snapshot.conversations.filter { (it.group && GroupLinks.isGroupId(it.peer)) || (!it.group && validAddress(it.peer)) }
                .take(MAX_CONVERSATIONS).map { it.copy(name = clean(it.name, NAME_CHARS) ?: displayName(it.peer), unread = it.unread.coerceAtLeast(0),
                    preview = if (snapshot.previews && !snapshot.locked && !it.group) clean(it.preview, PREVIEW_CHARS) else null) },
            nests = snapshot.nests?.let { n -> n.copy(nests = n.nests.take(MAX_NESTS)) },
            agenda = snapshot.agenda?.let { a -> a.copy(events = a.events.take(MAX_EVENTS)) },
            pushIds = snapshot.pushIds.takeLast(PUSH_IDS),
        )
        return if (clean.locked) redact(clean) else clean
    }

    /**
     * A message push for [mailbox] arrived while the app was closed: count it and move its
     * conversation to the top. The push carries no content, so the row says "New message" until the
     * app next runs. Returns null when nothing changes (another mailbox, a repeated push, a bad peer).
     */
    fun bump(snapshot: WidgetSnapshot, mailbox: String, peer: String, groupTitle: String?, messageId: String, now: Long): WidgetSnapshot? {
        if (snapshot.mailbox != mailbox || messageId.isBlank() || messageId.length > 200 || messageId in snapshot.pushIds) return null
        val group = GroupLinks.isGroupId(peer)
        if (!group && !validAddress(peer)) return null
        val ids = (snapshot.pushIds + messageId).takeLast(PUSH_IDS)
        if (snapshot.locked) return snapshot.copy(unread = snapshot.unread + 1, pushIds = ids, generatedAt = now)
        val existing = snapshot.conversations.firstOrNull { it.peer == peer }
        val row = existing?.copy(at = maxOf(existing.at, now), unread = existing.unread + 1, preview = null)
            ?: WidgetConversation(peer = peer, name = (if (group) clean(groupTitle, NAME_CHARS) ?: "Group" else displayName(peer)), at = now, unread = 1, group = group)
        val rows = (listOf(row) + snapshot.conversations.filterNot { it.peer == peer }).take(MAX_CONVERSATIONS)
        return snapshot.copy(unread = snapshot.unread + 1, conversations = rows, pushIds = ids, generatedAt = now)
    }
}
