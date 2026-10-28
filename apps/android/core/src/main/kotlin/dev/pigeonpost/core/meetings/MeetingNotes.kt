package dev.pigeonpost.core.meetings

import dev.pigeonpost.core.ApiException
import dev.pigeonpost.core.AuthorizationContext
import dev.pigeonpost.core.AuthorizationLease
import dev.pigeonpost.core.AuthorizationRetired
import dev.pigeonpost.core.Features
import dev.pigeonpost.core.SessionExpired
import dev.pigeonpost.core.WireJson
import dev.pigeonpost.core.groups.GroupTransport
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineDispatcher
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put

/**
 * Meeting notes (Phase 7, docs/meeting-summaries.md §1.5): the meetings whose notes this account agreed
 * to, opened on this device with its own notes key. The postbox holds only ciphertext and the content
 * key wrapped for each of the account's notes keys; this device registers its key while the feature
 * is on, as the web inbox's `meeting-notes.js` does, so notes made later can be opened here.
 */
@Serializable
data class MeetingRow(
    val id: String,
    val kind: String? = null,
    val state: String = "active",
    @SerialName("created_at") val createdAt: Long = 0,
    @SerialName("ended_at") val endedAt: Long? = null,
    @SerialName("ready_at") val readyAt: Long? = null,
    @SerialName("expires_at") val expiresAt: Long = 0,
    val host: String = "",
    @SerialName("as") val attendedAs: String? = null,
    val failure: String? = null,
)

@Serializable
data class MeetingsResponse(val meetings: List<MeetingRow>? = null)

@Serializable
data class MeetingWrap(@SerialName("key_id") val keyId: String, val epk: String, val iv: String, val wrapped: String)

@Serializable
data class MeetingDetail(
    val id: String,
    val kind: String? = null,
    val state: String = "active",
    @SerialName("created_at") val createdAt: Long = 0,
    @SerialName("ended_at") val endedAt: Long? = null,
    @SerialName("ready_at") val readyAt: Long? = null,
    @SerialName("expires_at") val expiresAt: Long = 0,
    val host: String = "",
    val failure: String? = null,
    val alg: String? = null,
    val iv: String? = null,
    val ciphertext: String? = null,
    val keys: List<MeetingWrap> = emptyList(),
)

@Serializable
data class ActionItem(val owner: String? = null, val task: String = "", val due: String? = null)
@Serializable
data class TranscriptLine(val at: String = "", val speaker: String = "", val text: String = "")
@Serializable
data class GeneratedBy(val stt: String? = null, val llm: String? = null)

/** The decrypted brief, as meeting-intel writes it. Everything in it came from speech: shown as plain text only. */
@Serializable
data class MeetingBrief(
    val v: Int = 1,
    @SerialName("meeting_id") val meetingId: String? = null,
    val title: String? = null,
    val language: String? = null,
    @SerialName("started_at") val startedAt: String? = null,
    @SerialName("duration_s") val durationSeconds: Long? = null,
    val participants: List<String> = emptyList(),
    val summary: String? = null,
    val decisions: List<String> = emptyList(),
    @SerialName("action_items") val actionItems: List<ActionItem> = emptyList(),
    @SerialName("open_questions") val openQuestions: List<String> = emptyList(),
    val draft: Boolean = false,
    val transcript: List<TranscriptLine> = emptyList(),
    @SerialName("generated_by") val generatedBy: GeneratedBy? = null,
) {
    /** "Copy as text", in the web reader's layout. */
    fun asText(): String = buildString {
        appendLine(title ?: "Meeting notes"); appendLine()
        summary?.let { appendLine(it); appendLine() }
        fun list(heading: String, items: List<String>) { if (items.isNotEmpty()) { appendLine("$heading:"); items.forEach { appendLine("- $it") }; appendLine() } }
        list("Decisions", decisions)
        list("Action items", actionItems.map(::actionText))
        list("Open questions", openQuestions)
        list("Transcript", transcript.map { "[${it.at}] ${it.speaker}: ${it.text}" })
    }.trimEnd() + "\n"

    companion object {
        fun actionText(a: ActionItem) = listOfNotNull(a.owner?.let { "$it:" }, a.task, a.due?.let { "(due $it)" }).joinToString(" ")
        /** At most 4 MiB of JSON, as the postbox stores; anything else is refused before parsing. */
        fun parse(bytes: ByteArray): MeetingBrief {
            if (bytes.size > 4 * 1024 * 1024) throw MeetingNotesError("bad_brief", "The notes are too large.")
            return try { WireJson.decodeFromString(serializer(), bytes.toString(Charsets.UTF_8)) }
            catch (_: Exception) { throw MeetingNotesError("bad_brief", "The notes could not be read.") }
        }
    }
}

/** The routes (`/v1/meetings…`, the person's own credential). */
interface MeetingNotesApi {
    suspend fun meetings(): List<MeetingRow>
    suspend fun meeting(id: String): MeetingDetail
    suspend fun delete(id: String)
    suspend fun registerKey(keyId: String, publicKey: String, label: String)
}

/**
 * Over the postbox client's group transport, which already carries the person's credential for any
 * `/v1/` path; the acting-mailbox header it adds is ignored by these account routes.
 */
class TransportMeetingNotesApi(private val transport: GroupTransport, private val mailbox: () -> String?) : MeetingNotesApi {
    private fun me() = mailbox() ?: throw AuthorizationRetired()
    override suspend fun meetings() = WireJson.decodeFromString<MeetingsResponse>(transport.groupJson(me(), "GET", "/v1/meetings")).meetings.orEmpty()
        .filter { MeetingNotesCrypto.MEETING_ID.matches(it.id) }
    override suspend fun meeting(id: String): MeetingDetail {
        require(MeetingNotesCrypto.MEETING_ID.matches(id))
        return WireJson.decodeFromString<MeetingDetail>(transport.groupJson(me(), "GET", "/v1/meetings/$id")).also {
            if (it.id != id) throw MeetingNotesError("bad_ciphertext", "Unexpected meeting.")
        }
    }
    override suspend fun delete(id: String) {
        require(MeetingNotesCrypto.MEETING_ID.matches(id))
        transport.groupJson(me(), "DELETE", "/v1/meetings/$id")
    }
    override suspend fun registerKey(keyId: String, publicKey: String, label: String) {
        require(MeetingNotesCrypto.validKeyId(keyId))
        transport.groupJson(me(), "PUT", "/v1/meetings/keys/$keyId", buildJsonObject { put("public_key", publicKey); put("label", label) })
    }
}

data class OpenedMeeting(val row: MeetingRow, val loading: Boolean = true, val brief: MeetingBrief? = null, val problem: String? = null)

data class MeetingNotesState(
    val contextId: String = java.util.UUID.randomUUID().toString(),
    val enabled: Boolean = false,
    val mailbox: String? = null,
    val meetings: List<MeetingRow> = emptyList(),
    val loading: Boolean = false,
    val loaded: Boolean = false,
    val error: String? = null,
    val opened: OpenedMeeting? = null,
    /** This device's notes key is registered for the account: notes made from now on open here. */
    val registered: Boolean = false,
    val notice: String? = null,
    val busy: Boolean = false,
)

object MeetingNotesText {
    fun state(row: MeetingRow) = when (row.state) {
        "active" -> "Notes in progress"
        "processing" -> "Being prepared…"
        "ready" -> "Ready"
        "failed" -> "Couldn’t be made"
        else -> row.state
    }
    fun failure(code: String?) = when (code) {
        "no_speech" -> "No speech was captured."
        "no_recipients" -> "No device could receive the notes."
        "stt_unavailable" -> "Transcription was unavailable."
        "timed_out" -> "The notes service didn’t finish in time."
        else -> "The notes couldn’t be made."
    }
    const val NO_KEY = "These notes were encrypted for your other devices. Open them on a device that was in the call, or one that opened Meeting notes before the call ended."
    const val CANNOT_OPEN = "This device couldn’t open these notes."
    const val INTRO = "Notes from calls where you agreed to them. They’re encrypted for your devices that were in the call or set up for notes; Pigeonpost’s servers keep only the encrypted copy, and delete it after the retention date."
}

/**
 * The Meeting notes screen's state. Registers this device's key once per mailbox binding while the
 * feature is on; every request runs under the binding's own authorization, so a sign-out or account
 * change retires work still in flight.
 */
class MeetingNotesStore(
    private val api: MeetingNotesApi,
    /** This device's key; made on first use. Called off the main thread. */
    private val keys: suspend () -> NotesKey?,
    private val scope: CoroutineScope,
    private val label: String = "Android app",
    private val onSessionExpired: () -> Unit = {},
    /** Key generation, agreement and decryption run here, never on the UI thread. */
    private val crypto: CoroutineDispatcher = Dispatchers.Default,
) {
    private val mutable = MutableStateFlow(MeetingNotesState())
    val state: StateFlow<MeetingNotesState> = mutable.asStateFlow()
    private var authorization: AuthorizationLease? = null
    private var generation = 0L
    private var openJob: Job? = null

    private fun launch(work: suspend CoroutineScope.() -> Unit): Job {
        val lease = authorization ?: return Job().apply { complete() }
        return scope.launch(AuthorizationContext(lease)) { lease.requireActive(); work() }
    }

    fun onMailbox(mailbox: String?, features: Set<String>) {
        val enabled = mailbox != null && Features.MEETING_SUMMARIES in features
        val current = state.value
        if (enabled && current.enabled && current.mailbox == mailbox) return
        reset()
        if (!enabled) return
        authorization = try { scope.coroutineContext[AuthorizationContext]?.lease?.child() ?: AuthorizationLease() } catch (_: AuthorizationRetired) { return }
        mutable.value = MeetingNotesState(enabled = true, mailbox = mailbox)
        register()
    }

    fun reset() {
        authorization?.retire(); authorization = null; generation++; openJob?.cancel()
        mutable.value = MeetingNotesState()
    }

    fun isCurrent(context: String) = state.value.enabled && state.value.contextId == context

    private fun register() {
        val run = generation
        launch {
            val key = try { withContext(crypto) { keys() } } catch (cancelled: CancellationException) { throw cancelled } catch (_: Exception) { null } ?: return@launch
            val ok = guarded(run) { api.registerKey(key.keyId, key.publicKey, label); true } == true
            if (run == generation && ok) mutable.update { it.copy(registered = true) }
        }
    }

    private suspend fun <T> guarded(run: Long, work: suspend () -> T): T? = try { work() }
        catch (cancelled: CancellationException) { throw cancelled }
        catch (expired: SessionExpired) { if (run == generation) onSessionExpired(); null }
        catch (_: Exception) { null }

    fun load() {
        if (!state.value.enabled) return
        val run = generation
        mutable.update { it.copy(loading = true, error = null) }
        if (!state.value.registered) register()
        launch {
            val list = guarded(run) { api.meetings() }
            if (run != generation) return@launch
            mutable.update { s ->
                if (list == null) s.copy(loading = false, error = "Couldn’t load your meeting notes.")
                else s.copy(loading = false, loaded = true, meetings = list.sortedByDescending { it.createdAt },
                    opened = s.opened?.let { o -> list.firstOrNull { it.id == o.row.id }?.let { o.copy(row = it) } ?: o })
            }
        }
    }

    fun open(row: MeetingRow) {
        val run = generation
        openJob?.cancel()
        mutable.update { it.copy(opened = OpenedMeeting(row)) }
        openJob = launch {
            val detail = try { api.meeting(row.id) }
                catch (cancelled: CancellationException) { throw cancelled }
                catch (expired: SessionExpired) { if (run == generation) onSessionExpired(); null }
                catch (_: Exception) { null }
            if (run != generation || state.value.opened?.row?.id != row.id) return@launch
            val result: OpenedMeeting = when {
                detail == null -> OpenedMeeting(row, loading = false, problem = "These notes are no longer available.")
                detail.state == "failed" -> OpenedMeeting(row.copy(state = detail.state), loading = false, problem = MeetingNotesText.failure(detail.failure))
                detail.state != "ready" -> OpenedMeeting(row.copy(state = detail.state), loading = false, problem = "The notes are still being prepared. Check back in a few minutes.")
                else -> try {
                    val brief = withContext(crypto) {
                        val key = keys() ?: throw MeetingNotesError("no_key", "No notes key on this device.")
                        val plain = MeetingNotesCrypto.open(detail, key)
                        try { MeetingBrief.parse(plain) } finally { plain.fill(0) }
                    }
                    OpenedMeeting(row.copy(state = detail.state), loading = false, brief = brief)
                } catch (cancelled: CancellationException) { throw cancelled }
                catch (failure: MeetingNotesError) { OpenedMeeting(row, loading = false, problem = if (failure.code == "no_key_for_device") MeetingNotesText.NO_KEY else MeetingNotesText.CANNOT_OPEN) }
                catch (_: Exception) { OpenedMeeting(row, loading = false, problem = MeetingNotesText.CANNOT_OPEN) }
            }
            if (run == generation && state.value.opened?.row?.id == row.id) mutable.update { it.copy(opened = result) }
        }
    }

    fun back() { openJob?.cancel(); mutable.update { it.copy(opened = null) } }

    fun delete(id: String, done: () -> Unit = {}) {
        val run = generation
        mutable.update { it.copy(busy = true) }
        launch {
            val ok = try { api.delete(id); true }
                catch (cancelled: CancellationException) { throw cancelled }
                catch (expired: SessionExpired) { if (run == generation) onSessionExpired(); false }
                catch (failure: ApiException) { failure.status == 404 }
                catch (_: Exception) { false }
            if (run != generation) return@launch
            mutable.update { s ->
                if (ok) s.copy(busy = false, opened = null, meetings = s.meetings.filterNot { it.id == id }, notice = "Deleted.")
                else s.copy(busy = false, notice = "Couldn’t delete.")
            }
            if (ok) done()
        }
    }

    fun clearNotice() = mutable.update { it.copy(notice = null) }

    /** An account event or push said notes changed: reread the list while the screen is open. */
    fun onBrief() { if (state.value.loaded) load() }
}
