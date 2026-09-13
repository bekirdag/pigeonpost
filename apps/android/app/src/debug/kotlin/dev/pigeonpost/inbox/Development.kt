package dev.pigeonpost.inbox

import android.app.Application
import android.content.Intent
import dev.pigeonpost.core.*
import dev.pigeonpost.inbox.auth.SessionState
import dev.pigeonpost.inbox.auth.UserSession
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import java.io.File
import java.io.IOException
import java.io.OutputStream
import java.util.UUID

/** Explicit opt-in for instrumentation and screenshots; ordinary debug launches use the real API. */
object Development {
    @Suppress("UNUSED_PARAMETER")
    fun graph(application: Application, intent: Intent): AppGraph? {
        val mode = intent.getStringExtra("pigeonpost.fixtures") ?: return null
        if (mode !in setOf("inbox", "empty", "offline", "signin", "long", "handles", "policy")) return null
        return AppGraph(FixtureSession(mode != "signin", mode != "policy" && mode != "signin"), FixturePostbox(mode), fixtures = true)
    }
}
private class FixtureSession(signedIn: Boolean, accepted: Boolean) : UserSession {
    override val state = MutableStateFlow(SessionState(loading = false, signedIn = signedIn, username = "demo", termsAccepted = signedIn && accepted))
    override suspend fun begin(provider: String?, otherAccount: Boolean): Intent? { state.value = state.value.copy(signedIn = true, termsAccepted = false); return null }
    override suspend fun complete(result: Intent?) {}
    override suspend fun cancel() {}
    override suspend fun acceptTerms() { state.value = state.value.copy(termsAccepted = state.value.signedIn) }
    override suspend fun signOut() { state.value = state.value.copy(signedIn = false, termsAccepted = false) }
    override suspend fun token(rejected: String?) = "fixture-only"
}

private class FixturePostbox(private val mode: String) : PostboxApi, AccountHandleApi {
    private var created = mode != "empty"
    private val now = System.currentTimeMillis() / 1000
    private val boxes = mutableListOf(IdentityRow("/k/demo-main", "Main"), IdentityRow("/k/demo-agent", "Build agent"))
    private var preview: HandleOffer = HandleOffer(eligible = mode == "handles")
    private val messages = mutableListOf(
        Message("m_1", "The Android build is ready for review.\n\n- Native Kotlin and Compose\n- The same conversations and subjects\n- Files stay with each message", from = "/k/demo-agent", peer = "/k/demo-agent", peerHandle = "/demo/builder", receivedAt = now - 70, read = false, autonomy = "auto", verb = "report_status", threadId = "t_build"),
        Message("m_2", workEnvelope("Please run the Android tests and send me the results."), to = "/demo/builder", peerHandle = "/demo/builder", direction = "out", sentAt = now - 160, threadId = "t_build"),
        Message("m_3", "The inbox layout follows iOS, with native Android navigation.", from = "/k/demo-agent", peerHandle = "/demo/builder", receivedAt = now - 600, read = true, threadId = "t_design"),
        Message("m_4", "{\"v\":1,\"verb\":\"run_tests\",\"note\":\"May I run the updated test suite?\"}", from = "/k/reviewer", peerHandle = "/demo/reviewer", receivedAt = now - 260, read = false, autonomy = "review", verb = "run_tests", heldBecause = "verb_denied", standing = "good", tier = "named"),
        Message("m_5", "# Build notes\n\nEverything is ready to inspect.\n\n```kotlin\nval platform = \"Android\"\n```\n\nRead the [project website](https://pigeonpost.dev).", from = "/k/docdex", peerHandle = "/demo/docdex", receivedAt = now - 4800, read = true,
            attachments = listOf(Attachment("a_notes", "build-notes.txt", "text/plain", 35))),
    )
    private val subjects = mutableListOf(ServerThread("t_build", "/demo/builder", "Android build", lastAt = now), ServerThread("t_design", "/demo/builder", "Design", lastAt = now - 600))
    private val people = mutableListOf(Contact("/demo/builder", "Build agent", "allow", "auto", listOf("report_status")), Contact("/demo/docdex", "Docdex"), Contact("/demo/archived", "Archived agent"))
    private val archived = mutableSetOf("/demo/archived")
    private val attachments = mutableMapOf("a_notes" to "Pigeonpost Android development build".toByteArray())
    init {
        if (mode == "long") repeat(1000) { index -> messages += Message("history_$index", "History message $index\n\nA repeatable scrolling check.", from = "/k/demo-agent", peerHandle = "/demo/builder", threadId = "t_build", receivedAt = now - 10000 + index, read = true) }
    }
    override suspend fun identities() = if (created) boxes else emptyList()
    override suspend fun accountHandles() = listOf(
        AccountHandle("demo", "apple", now + 86400, true),
        AccountHandle("studio", "google", now + 86400, true),
        AccountHandle("previous", "google", now - 86400, false),
    )
    override suspend fun whoami(identity: String) = WhoAmI(identity, if (identity == "/k/preview") preview.mailbox else if (identity == boxes.first().address) "/demo/main" else "/demo/builder")
    override suspend fun createIdentity(handle: String?): String { created = true; return boxes.first().address }
    override suspend fun inbox(identity: String, wait: Int?): InboxResponse {
        if (mode == "offline") throw IOException("You’re offline. Check your connection and try again.")
        if (wait != null) delay(wait * 1000L)
        return InboxResponse(if (identity == boxes.first().address) messages.toList() else emptyList())
    }
    override suspend fun threads(identity: String) = if (identity == boxes.first().address) subjects.toList() else emptyList()
    override suspend fun contacts(identity: String) = ContactsResponse(if (identity == boxes.first().address) people.toList() else emptyList(), Vocabulary(listOf("report_status", "answer_question", "read_file", "run_tests", "run_shell"), listOf("run_shell", "git_push", "deploy", "read_credentials", "spend", "delete_files")))
    override suspend fun archive(identity: String) = archived.toSet()
    override suspend fun quota(identity: String) = Quota(23 * 1024 * 1024, 250 * 1024 * 1024, 200 * 1024 * 1024, "Free")
    override suspend fun handleOffer() = preview
    override suspend fun checkHandle(name: String) = HandleAvailability(name, name !in setOf("support", "taken"), if (name == "support") "reserved" else "taken")
    override suspend fun claimHandle(name: String): HandleOffer {
        check(preview.eligible)
        preview = HandleOffer("/$name", eligible = true, mailbox = "/$name/main", source = "test_preview")
        if (boxes.none { it.address == "/k/preview" }) boxes += IdentityRow("/k/preview")
        return preview
    }
    override suspend fun send(identity: String, to: String, body: String, threadId: String?, attachments: List<String>): SendResponse {
        delay(150)
        val id = "sent_" + UUID.randomUUID()
        messages += Message(id, body, to = to, direction = "out", peerHandle = to, threadId = threadId, sentAt = System.currentTimeMillis() / 1000,
            attachments = attachments.map { Attachment(it, "attachment.txt", "text/plain", this.attachments[it]?.size?.toLong() ?: 0) })
        return SendResponse("delivered_" + id, id)
    }
    override suspend fun ack(identity: String, messageId: String) { val index = messages.indexOfFirst { it.id == messageId }; if (index >= 0) messages[index] = messages[index].copy(read = true) }
    override suspend fun openThread(identity: String, peer: String, title: String): String { val id = "thread_" + UUID.randomUUID(); subjects += ServerThread(id, peer, title, lastAt = System.currentTimeMillis() / 1000); return id }
    override suspend fun deleteThread(identity: String, id: String) { subjects.removeAll { it.id == id }; messages.removeAll { it.threadId == id } }
    override suspend fun deleteMessage(identity: String, id: String) { messages.removeAll { it.id == id } }
    override suspend fun reportSpam(identity: String, id: String) {}
    override suspend fun setArchived(identity: String, peer: String, archived: Boolean) { if (archived) this.archived += peer else this.archived -= peer }
    override suspend fun saveContact(identity: String, contact: Contact) { people.removeAll { it.peer == contact.peer }; people += contact }
    override suspend fun removeContact(identity: String, peer: String) { people.removeAll { it.peer == peer } }
    override suspend fun upload(identity: String, file: File, filename: String, mediaType: String): Attachment { val id = "a_" + UUID.randomUUID(); attachments[id] = file.readBytes(); return Attachment(id, filename, mediaType, file.length()) }
    override suspend fun download(identity: String, id: String, output: OutputStream, maximumBytes: Long) { output.write(attachments[id] ?: error("Attachment not found")) }
}
