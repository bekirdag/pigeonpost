package dev.pigeonpost.inbox

import android.app.Application
import android.content.Intent
import dev.pigeonpost.core.*
import dev.pigeonpost.inbox.auth.SessionState
import dev.pigeonpost.inbox.auth.UserSession
import dev.pigeonpost.inbox.calls.CallsOverride
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
        if (mode !in setOf("inbox", "empty", "offline", "signin", "long", "long-tall", "handles", "policy", "store")) return null
        return AppGraph(FixtureSession(mode != "signin", mode != "policy" && mode != "signin"), FixturePostbox(mode), fixtures = true)
    }

    /**
     * Local call tests only, like the iOS app's `-calls-page/-calls-token/-calls-mailbox/-calls-handle/-call`:
     * `--es pigeonpost.calls.page http://127.0.0.1:8460/native-call.html --es pigeonpost.calls.token tok-alice
     *  --es pigeonpost.calls.mailbox /k/alice… [--es pigeonpost.calls.handle /alice] [--es pigeonpost.call /bob:video]`.
     * Only the debug source set has this; release builds cannot be pointed anywhere but production.
     */
    fun calls(intent: Intent): CallsOverride? {
        val page = intent.getStringExtra("pigeonpost.calls.page") ?: return null
        return CallsOverride(page, intent.getStringExtra("pigeonpost.calls.token"), intent.getStringExtra("pigeonpost.calls.mailbox"),
            intent.getStringExtra("pigeonpost.calls.handle"), intent.getStringExtra("pigeonpost.call"))
    }

    /** A screenshot run may pin light or dark without touching the saved setting. */
    fun appearance(intent: Intent): String? = intent.getStringExtra("pigeonpost.appearance")

    /** Lets the call page be inspected from the computer (chrome://inspect) in debug builds. */
    fun started() { android.webkit.WebView.setWebContentsDebuggingEnabled(true) }
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
    private val attachmentMetadata = mutableMapOf<String, Attachment>()
    private val attachments = mutableMapOf("a_notes" to "Pigeonpost Android development build".toByteArray())
    init {
        if (mode == "long" || mode == "long-tall") repeat(1000) { index -> messages += Message("history_$index", "History message $index\n\nA repeatable scrolling check.", from = "/k/demo-agent", peerHandle = "/demo/builder", threadId = "t_build", receivedAt = now - 10000 + index, read = true) }
        if (mode == "store") storeFixture()
        if (mode == "long-tall") messages[0] = messages[0].copy(body =
            "# A long build report\n\n" + (1..60).joinToString("\n\n") { "Section $it: verifying native message layout and scrolling." } + "\n\nNewest report footer")
    }
    /** Store screenshots: the same fleet the iOS App Store shots show. Invented addresses, no real data. */
    private fun storeFixture() {
        boxes.clear(); boxes += listOf(IdentityRow("/k/cz6900v2h90vnwefj7g7ezvbh4", "main"), IdentityRow("/k/zz1111v2h90vnwefj7g7ezvbh9", "docdex box"), IdentityRow("/k/qq2222v2h90vnwefj7g7ezvbh7", "scratch"))
        val agent = "/bekir/agent1"
        messages.clear(); messages += listOf(
            Message("m1", "the build is green — 0.5.21 tagged and the npm job went through unattended", from = "/k/aaaa1111bbbb2222cccc3333dd", peer = agent, peerHandle = agent, senderHandle = agent,
                threadId = "t-agent1", direction = "in", autonomy = "review", heldBecause = "not_a_request", receivedAt = now - 7200, read = true),
            Message("m_out1", "good. run the unit suite once more before the release gate", direction = "out", to = agent, peer = agent, peerHandle = agent, threadId = "t-agent1", sentAt = now - 3600, read = true),
            Message("m2", "{\"v\":1,\"verb\":\"run_tests\",\"args\":{\"suite\":\"unit\",\"target\":\"crates/pigeonpost-postbox\"},\"note\":\"before the tag\"}",
                from = "/k/aaaa1111bbbb2222cccc3333dd", peer = agent, peerHandle = agent, senderHandle = agent, threadId = "t-agent1", direction = "in", autonomy = "review", verb = "run_tests",
                heldBecause = "verb_denied", receivedAt = now - 900, read = false),
            Message("m_out2", "pin the flaky macOS test and push a fix", direction = "out", to = agent, peer = agent, peerHandle = agent, threadId = "t-agent1", sentAt = now - 600, read = true),
            Message("m_reply", "pigeonpost-auto-reply v1 in_reply_to=m_out2 answered=full_access\nGenerated unattended by this mailbox's agent. Nobody read it before it was sent.\n\n## Pinned it\n\n" +
                "The fixture listener was **non-blocking**, which macOS inherits onto the accepted socket and Linux does not.\n\n- `set_nonblocking(false)` in `serve_http_request`\n- one test added for the accepted socket\n\n" +
                "```rust\nstream.set_nonblocking(false)?;\n```\n\nPushed as `4f21c0e` and CI is green: fmt, clippy, and the Linux and macOS test jobs all pass.",
                from = "/k/aaaa1111bbbb2222cccc3333dd", peer = agent, peerHandle = agent, senderHandle = agent, threadId = "t-agent1", direction = "in", autonomy = "auto", verb = "full_access", receivedAt = now - 420, read = true),
            Message("m_docdex", "index rebuilt — 41k symbols, 1.2s", from = "/k/zz1111v2h90vnwefj7g7ezvbh9", peer = "/bekir/docdex", peerHandle = "/bekir/docdex", senderHandle = "/bekir/docdex",
                direction = "in", autonomy = "auto", receivedAt = now - 108000, read = true),
            Message("m3", "hello — are you the maintainer of pigeonpost?", from = "/k/eeee5555ffff6666gggg7777hh", peer = "/k/eeee5555ffff6666gggg7777hh", threadId = "t-stranger",
                direction = "in", autonomy = "review", heldBecause = "sender_not_auto", receivedAt = now - 240, read = false),
        )
        subjects.clear(); subjects += listOf(ServerThread("t-agent1", agent, null, isDefault = true, lastAt = now - 900), ServerThread("t-agent1-deploy", agent, "the deploy", lastAt = now - 8000),
            ServerThread("t-stranger", "/k/eeee5555ffff6666gggg7777hh", null, isDefault = true, lastAt = now - 240))
        people.clear(); people += Contact("/bekir/*", "my fleet", "allow", "review", emptyList())
        archived.clear()
    }
    override suspend fun identities() = if (created) boxes else emptyList()
    override suspend fun accountHandles() = listOf(
        AccountHandle("demo", "apple", now + 86400, true),
        AccountHandle("studio", "google", now + 86400, true),
        AccountHandle("previous", "google", now - 86400, false),
    )
    override suspend fun whoami(identity: String) = WhoAmI(identity, when {
        mode == "store" -> mapOf("/k/cz6900v2h90vnwefj7g7ezvbh4" to "/bekir/main", "/k/zz1111v2h90vnwefj7g7ezvbh9" to "/bekir/docdex")[identity]
        identity == "/k/preview" -> preview.mailbox
        identity == boxes.first().address -> "/demo/main"
        else -> "/demo/builder"
    })
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
            attachments = attachments.map { attachmentMetadata[it] ?: Attachment(it, "attachment.txt", "text/plain", this.attachments[it]?.size?.toLong() ?: 0) })
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
    override suspend fun upload(identity: String, file: File, filename: String, mediaType: String): Attachment { val id = "a_" + UUID.randomUUID(); attachments[id] = file.readBytes(); return Attachment(id, filename, mediaType, file.length()).also { attachmentMetadata[id] = it } }
    override suspend fun download(identity: String, id: String, output: OutputStream, maximumBytes: Long) { output.write(attachments[id] ?: error("Attachment not found")) }
}
