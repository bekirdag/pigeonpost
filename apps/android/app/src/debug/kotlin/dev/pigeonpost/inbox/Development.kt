package dev.pigeonpost.inbox

import android.app.Application
import android.content.Context
import android.content.Intent
import dev.pigeonpost.core.*
import dev.pigeonpost.inbox.auth.SessionState
import dev.pigeonpost.inbox.auth.UserSession
import dev.pigeonpost.inbox.calls.CallsOverride
import kotlinx.coroutines.delay
import kotlinx.coroutines.isActive
import kotlinx.coroutines.launch
import kotlinx.serialization.json.put
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
        if (mode !in setOf("inbox", "empty", "offline", "signin", "long", "long-tall", "handles", "policy", "store", "safety", "groups", "actions", "actions-lost", "actions-off", "group-actions", "group-actions-lost", "group-actions-off", "calendar", "calendar-lost", "calendar-conflict")) return null
        val session = FixtureSession(mode != "signin", mode != "policy" && mode != "signin", scoped = mode.startsWith("calendar"))
        if (mode.startsWith("calendar")) return AppGraph(session, CalendarFixture(FixturePostbox("inbox"), mode), fixtures = true,
            calendarStorage = { dev.pigeonpost.core.calendar.MemoryCalendarAttemptStorage() })
        if (mode.startsWith("actions")) {
            val base = FixturePostbox("inbox")
            return AppGraph(session, MessageActionFixture(mode, base, base), fixtures = true)
        }
        if (mode.startsWith("group-actions")) {
            val world = FixtureGroups(answers = false, actionMessages = true)
            val base = FixturePostbox("groups", world.me)
            val actions = GroupActionFixture(mode, base, base, world)
            return AppGraph(session, actions, fixtures = true, groupTransport = actions.transport,
                groupCrypto = { world.crypto }, groupStorage = { dev.pigeonpost.core.groups.MemoryGroupStorage() }, start = world::run, groupPollSeconds = 1)
        }
        if (mode == "groups") {
            // Real group encryption (the AAR) against the in-memory groups postbox, with two other people.
            val world = FixtureGroups()
            return AppGraph(session, FixturePostbox(mode, world.me), fixtures = true, groupTransport = world.fake.transport,
                groupCrypto = { world.crypto }, groupStorage = { dev.pigeonpost.core.groups.MemoryGroupStorage() }, start = world::run, groupPollSeconds = 1)
        }
        return AppGraph(session, FixturePostbox(mode), fixtures = true)
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

    private val harness = listOf("pigeonpost.fixtures", "pigeonpost.calls.page", "pigeonpost.calls.token", "pigeonpost.calls.mailbox", "pigeonpost.calls.handle")

    /**
     * Incoming-call tests start with the app killed, so a launch with the call harness is remembered:
     * a test push (DebugPushReceiver) then rings for the harness mailbox against the harness origin, and
     * answering reopens the app into the same harness. An ordinary launch forgets it.
     */
    fun rememberHarness(context: Context, intent: Intent) {
        val preferences = context.getSharedPreferences("debug-calls", Context.MODE_PRIVATE)
        val page = intent.getStringExtra("pigeonpost.calls.page")
        if (page == null) { if (intent.action == Intent.ACTION_MAIN) preferences.edit().clear().apply(); return }
        val uri = android.net.Uri.parse(page)
        preferences.edit().clear().apply {
            harness.forEach { key -> intent.getStringExtra(key)?.let { putString(key, it) } }
            putString("identity", intent.getStringExtra("pigeonpost.calls.mailbox"))
            putString("origin", "${uri.scheme}://${uri.authority}")
        }.apply()
    }

    /** The remembered harness, on an intent that reopens the app (answering a call). */
    fun harnessExtras(context: Context, intent: Intent): Intent {
        val preferences = context.getSharedPreferences("debug-calls", Context.MODE_PRIVATE)
        harness.forEach { key -> preferences.getString(key, null)?.let { intent.putExtra(key, it) } }
        return intent
    }
    fun callIdentity(context: Context): String? = context.getSharedPreferences("debug-calls", Context.MODE_PRIVATE).getString("identity", null)
    fun callsOrigin(context: Context): String? = context.getSharedPreferences("debug-calls", Context.MODE_PRIVATE).getString("origin", null)

    /** Lets the call page be inspected from the computer (chrome://inspect) in debug builds. */
    fun started() { android.webkit.WebView.setWebContentsDebuggingEnabled(true) }
}
private class FixtureSession(signedIn: Boolean, accepted: Boolean, scoped: Boolean = false) : UserSession {
    private val grant = if (scoped) AuthorizationLease() else null
    override val authorizationLease get() = grant
    override val state = MutableStateFlow(SessionState(loading = false, signedIn = signedIn, username = "demo", termsAccepted = signedIn && accepted, authorizationId = grant?.id))
    override suspend fun begin(provider: String?, otherAccount: Boolean): Intent? { state.value = state.value.copy(signedIn = true, termsAccepted = false); return null }
    override suspend fun complete(result: Intent?) {}
    override suspend fun cancel() {}
    override suspend fun acceptTerms() { state.value = state.value.copy(termsAccepted = state.value.signedIn) }
    override suspend fun signOut() { grant?.retire(); state.value = state.value.copy(signedIn = false, termsAccepted = false, authorizationId = null) }
    override suspend fun token(rejected: String?) = "fixture-only"
}

private class FixturePostbox(private val mode: String, groupsMailbox: String? = null) : PostboxApi, AccountHandleApi {
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
    init { groupsMailbox?.let { boxes[0] = IdentityRow(it, "Main") } }
    override suspend fun identities() = if (created) boxes else emptyList()

    // ---- Phase 1b fixtures (mode "safety") and groups (mode "groups") ---------------------------
    private val safety = mode == "safety"
    override suspend fun capabilities() = Capabilities(when (mode) {
        "safety" -> listOf("call_history", "call_admission", "message_filtering", "avatars", "disappearing")
        "groups" -> listOf("groups", "disappearing")
        else -> emptyList()
    })
    private var calls = listOf(
        CallLogEntry("call_missed", "incoming", "missed", "video", "/k/demo-agent", "/demo/builder", startedAt = now - 120),
        CallLogEntry("call_out", "outgoing", "answered", "audio", "/k/reviewer", "/demo/reviewer", startedAt = now - 7200, answeredAt = now - 7190, endedAt = now - 7065),
    )
    private var unseen = 1
    private val filteredCalls = listOf(CallLogEntry("call_filtered", "incoming", "filtered", "audio", "/k/stranger7ab3c9d0e1f2a3b4c5", startedAt = now - 300))
    private var filtered = listOf(
        Message("f_1", "Hi! Are you hiring agents?", from = "/k/stranger7ab3c9d0e1f2a3b4c5", peer = "/k/stranger7ab3c9d0e1f2a3b4c5", receivedAt = now - 900, filtered = true),
        Message("f_2", "Following up on my earlier message.", from = "/k/stranger7ab3c9d0e1f2a3b4c5", peer = "/k/stranger7ab3c9d0e1f2a3b4c5", receivedAt = now - 600, filtered = true),
    )
    private var policy = FilterPolicy("ring", "silence", "show", "filter", "filter", 180)
    private var own: AvatarInfo? = null
    val reports = mutableListOf<String>()
    override suspend fun callLog(identity: String, filter: String, before: Long?, limit: Int): CallLogPage {
        if (!safety) throw ApiException(404, "feature_disabled")
        return CallLogPage(when (filter) { "filtered" -> filteredCalls; "missed" -> calls.filter { it.missed }; else -> calls }, unseen)
    }
    override suspend fun markCallsSeen(identity: String, upTo: Long): Int { unseen = 0; return 0 }
    override suspend fun deleteCall(identity: String, id: String) { calls = calls.filterNot { it.id == id } }
    override suspend fun clearCalls(identity: String) { calls = emptyList() }
    override suspend fun reportCall(identity: String, callId: String) { reports += "call:$callId" }
    override suspend fun filteredInbox(identity: String) = if (safety) filtered else emptyList()
    override suspend fun acceptFiltered(identity: String, peer: String) {
        messages += filtered.map { it.copy(filtered = false, read = true) }; filtered = emptyList()
        people += Contact(peer)
    }
    override suspend fun policy(identity: String) = policy
    override suspend fun setPolicy(identity: String, field: String, value: String) = policy.with(field, value).also { policy = it }
    override suspend fun blockedContacts(identity: String) = people.filter { it.admission == "block" }
    override suspend fun unblock(identity: String, peer: String) { people.removeAll { it.peer == peer } }
    override suspend fun avatars(keys: List<String>): Map<String, AvatarInfo?> = if (!safety) emptyMap() else keys.associateWith { key ->
        when (key) {
            "/demo/builder" -> AvatarInfo("av_builder", 1, "h_builder", "everyone", AvatarUrls(medium = "/v1/avatar-blobs/av_builder/1/128.png"))
            boxes.first().address, "/demo/main" -> own
            else -> null
        }
    }
    override suspend fun avatarBytes(path: String): ByteArray = fixturePicture(if (path.contains("builder")) 0xFF2F6FEB.toInt() else 0xFF1AA179.toInt())
    override suspend fun ownAvatar(identity: String) = own
    override suspend fun setAvatar(identity: String, image: ByteArray, mediaType: String, crop: CropRect, visibility: String, all: Boolean): AvatarInfo? {
        check(image.size > 100 && mediaType == "image/jpeg")
        own = AvatarInfo("av_own", (own?.version ?: 0) + 1, "h_own_${(own?.version ?: 0) + 1}", visibility, AvatarUrls(medium = "/v1/avatar-blobs/av_own/1/128.png"))
        return own
    }
    override suspend fun setAvatarVisibility(identity: String, visibility: String) = own?.copy(visibility = visibility).also { own = it }
    override suspend fun removeAvatar(identity: String) { own = null }
    override suspend fun reportAvatar(identity: String, address: String) { reports += "avatar:$address" }
    override suspend fun setThreadTimer(identity: String, threadId: String, seconds: Long?) {
        val i = subjects.indexOfFirst { it.id == threadId }
        if (i >= 0) subjects[i] = subjects[i].copy(ephemeralSecs = seconds)
    }
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

/** A solid square PNG, standing in for a profile picture. */
private fun fixturePicture(color: Int): ByteArray {
    val bitmap = android.graphics.Bitmap.createBitmap(64, 64, android.graphics.Bitmap.Config.ARGB_8888).apply { eraseColor(color) }
    return java.io.ByteArrayOutputStream().also { bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it) }.toByteArray()
}

/**
 * Groups fixture: the app's mailbox plus Bob and Carol, each a real group-crypto device, on the
 * in-memory groups postbox (core/src/groupcrypto-fake). Bob owns "Garden club" (Carol is in it and the
 * app's mailbox is accepted, waiting to be added); Carol has invited the app's mailbox to "Book club".
 * While the app runs, Bob and Carol add pending devices, accept invitations and answer messages.
 */
class FixtureGroups(private val answers: Boolean = true, private val actionMessages: Boolean = false) {
    val fake = dev.pigeonpost.core.groups.FakeGroupPostbox().apply { serialize = true }
    val crypto = dev.pigeonpost.core.groups.UniffiGroupCrypto()
    val me = fake.mailbox()
    private val bob = fake.mailbox()
    private val carol = fake.mailbox()
    private val people = mutableListOf<dev.pigeonpost.core.groups.GroupClient>()
    private val answered = mutableSetOf<Long>()
    private var actionSeeded = false
    init { fake.handles[me] = "/demo/main"; fake.handles[bob] = "/bob/main"; fake.handles[carol] = "/carol/main" }

    fun run(scope: kotlinx.coroutines.CoroutineScope) {
        scope.launch(kotlinx.coroutines.Dispatchers.Default) {
            val b = dev.pigeonpost.core.groups.GroupClient(crypto, fake.transport, dev.pigeonpost.core.groups.MemoryGroupStorage(), bob, "bob", jitter = { 0 })
            val c = dev.pigeonpost.core.groups.GroupClient(crypto, fake.transport, dev.pigeonpost.core.groups.MemoryGroupStorage(), carol, "carol", jitter = { 0 })
            for (x in listOf(b, c)) { x.init(); x.topUpKeyPackages() }
            people += b; people += c
            val garden = b.createGroup(kotlinx.serialization.json.buildJsonObject { put("name", "Garden club"); put("description", "Seeds, soil and harvests") }).group.id
            fake.invite(garden, carol); fake.accept(garden, carol)
            b.processPending(garden, noJitter = true); c.processWelcomes()
            b.send(garden, dev.pigeonpost.core.groups.OutgoingDocument("Tomatoes are in!"))
            fake.invite(garden, me); fake.accept(garden, me)
            val book = c.createGroup(kotlinx.serialization.json.buildJsonObject { put("name", "Book club"); put("post_policy", "admins") }).group.id
            fake.invite(book, me)
            while (isActive) {
                for (x in people) try {
                    x.processWelcomes()
                    for ((gid, g) in fake.groups) {
                        val member = g.members[x.mailbox] ?: continue
                        if (member.state == "invited") fake.accept(gid, x.mailbox)
                        if (!x.inGroup(gid)) continue
                        x.sync(gid)
                        x.processPending(gid, noJitter = true)
                    }
                } catch (cancelled: kotlinx.coroutines.CancellationException) { throw cancelled } catch (_: Exception) {}
                // Bob answers the app's messages in the groups he is in.
                try {
                    if (actionMessages && !actionSeeded && fake.groups[garden]?.members?.get(me)?.state == "active") {
                        b.send(garden, dev.pigeonpost.core.groups.OutgoingDocument("The group release plan is ready.")); actionSeeded = true
                    }
                    if (answers) for (gid in fake.groups.keys) for (item in b.history(gid)) {
                        val seq = item.seq ?: continue
                        if (item.kind == "message" && item.sender == me && !item.deleted && seq !in answered) {
                            answered += seq
                            b.send(gid, dev.pigeonpost.core.groups.OutgoingDocument("Got it: " + item.text.orEmpty()))
                        }
                    }
                } catch (cancelled: kotlinx.coroutines.CancellationException) { throw cancelled } catch (_: Exception) {}
                delay(700)
            }
        }
    }
}
