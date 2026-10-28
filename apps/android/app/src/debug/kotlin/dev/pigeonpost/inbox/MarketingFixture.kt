package dev.pigeonpost.inbox

import android.content.Intent
import dev.pigeonpost.core.*
import dev.pigeonpost.core.calendar.*
import dev.pigeonpost.core.enterprise.*
import dev.pigeonpost.core.groups.*
import dev.pigeonpost.core.meetings.MeetingNotesCrypto
import dev.pigeonpost.inbox.auth.SessionState
import dev.pigeonpost.inbox.auth.UserSession
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.serialization.json.*
import java.time.Instant
import java.time.LocalDate
import java.time.ZoneId
import java.util.UUID

/**
 * Debug fixture `marketing`: the invented world of marketing/store/seed/demo-world.json, for the store
 * screenshots. Every person, agent, group, organization, message and meeting is fictional; nothing here
 * makes a network request. Groups are real MLS (the AAR) on the in-memory groups postbox, as in the
 * `groups` and `live` fixtures. Debug source set only.
 */
object MarketingControl {
    @Volatile var current: MarketingFixture? = null
}

internal class MarketingSession : UserSession {
    val grant = AuthorizationLease()
    override val authorizationLease get() = grant
    override val state = MutableStateFlow(SessionState(loading = false, signedIn = true, termsAccepted = true, username = "maya",
        authorizationId = grant.id, subject = "fixture-member"))
    override suspend fun token(rejected: String?) = "fixture-only"
    override suspend fun begin(provider: String?, otherAccount: Boolean): Intent? = null
    override suspend fun complete(result: Intent?) {}
    override suspend fun cancel() {}
    override suspend fun acceptTerms() {}
    override suspend fun signOut() {}
}

/** In memory, but reported as kept: a screenshot should not carry the fixture-only "cannot keep keys" notice. */
internal class MarketingGroupStorage(private val memory: MemoryGroupStorage = MemoryGroupStorage()) : GroupStorage by memory {
    override val persistent = true
}

class MarketingFixture internal constructor(private val base: PostboxApi, val fake: FakeGroupPostbox, val me: String) :
    PostboxApi by base, AccountHandleApi by (base as AccountHandleApi), GroupTransport, CalendarTransport, CalendarMeetingApi {

    companion object {
        internal fun graph(postbox: (String) -> PostboxApi): AppGraph {
            val fake = FakeGroupPostbox().apply { serialize = true }
            val me = fake.mailbox()
            fake.handles[me] = "/maya"
            val fixture = MarketingFixture(postbox(me), fake, me).also { MarketingControl.current = it }
            return AppGraph(MarketingSession(), fixture, fixtures = true, groupTransport = fixture, groupCrypto = { fixture.crypto },
                groupStorage = { MarketingGroupStorage() }, start = fixture::run, groupPollSeconds = 1,
                calendarStorage = { MemoryCalendarAttemptStorage() }, streamPollMillis = 1000)
        }
    }

    val crypto = UniffiGroupCrypto()
    private val now = System.currentTimeMillis() / 1000
    private fun ago(minutes: Int) = now - minutes * 60L

    // ---- People and agents ------------------------------------------------------------------------
    private val names = linkedMapOf(
        "/leo" to "Leo", "/ana" to "Ana", "/sam" to "Sam", "/priya" to "Priya", "/noor" to "Noor", "/kai" to "Kai", "/tomas" to "Tomás",
        "/maya/research-agent" to "Research agent", "/maya/trip-planner" to "Trip planner",
    )
    private val colors = mapOf(
        "/maya" to 0xFF2563EB, "/leo" to 0xFF16A34A, "/ana" to 0xFFDB2777, "/sam" to 0xFFEA580C, "/priya" to 0xFF7C3AED,
        "/noor" to 0xFF0891B2, "/kai" to 0xFFCA8A04, "/tomas" to 0xFF475569, "/maya/research-agent" to 0xFF0F766E, "/maya/trip-planner" to 0xFF4338CA,
    )
    private val mailboxes = linkedMapOf<String, String>() // handle -> fake mailbox

    // ---- Direct conversations ---------------------------------------------------------------------
    private val agent = "/maya/research-agent"
    private fun ref(n: Int) = "mref_" + ('a' + n).toString().repeat(26)
    private fun inMsg(id: String, peer: String, body: String, min: Int, read: Boolean = true, n: Int = 0) =
        Message(id, body, from = peer, peer = peer, peerHandle = peer, senderHandle = peer, direction = "in", receivedAt = ago(min), read = read,
            threadId = "t_$peer", msgRef = ref(n), autonomy = "auto", standing = "good", tier = "named")
    private fun outMsg(id: String, peer: String, body: String, min: Int, receipt: MessageReceipt, n: Int = 0, status: MessageAgentStatus? = null) =
        Message(id, workEnvelope(body), to = peer, peer = peer, peerHandle = peer, direction = "out", sentAt = ago(min), read = true,
            threadId = "t_$peer", msgRef = ref(n), receipt = receipt, agentStatus = status)

    private val messages = mutableListOf(
        outMsg("ra_1", agent, "Can you compare the three venues for the team offsite in May? 20 people, two days.", 26,
            MessageReceipt("seen", ago(26), ago(25)), 1),
        inMsg("ra_2", agent, "## Venue shortlist\n\n" +
            "1. **Harbor Hall** — 24 seats · €1,450 a day · 5 min walk from the station\n" +
            "2. **Mill House** — 30 seats · €1,900 a day · 20 min by bus\n" +
            "3. **The Glasshouse** — 18 seats · €1,200 a day · 10 min walk\n\n" +
            "**Harbor Hall** is the best fit: room for everyone, quiet breakout spaces and lunch included.", 19, n = 2),
        outMsg("ra_3", agent, "Great — hold Harbor Hall for May 14–15 and send Leo the details.", 3,
            MessageReceipt("delivered", ago(3)), 3, MessageAgentStatus("working", ago(2))),

        inMsg("leo_1", "/leo", "Hey! Are we still on for Friday?", 40, n = 4),
        outMsg("leo_2", "/leo", "Yes! 7:30 works for me.", 38, MessageReceipt("seen", ago(38), ago(37)), 5),
        inMsg("leo_3", "/leo", "Great, I'll book a table for two.", 7, read = false, n = 6),
        inMsg("leo_4", "/leo", "Dinner Friday? I found a great ramen place 🍜", 6, read = false, n = 7),

        inMsg("ana_1", "/ana", "Photos from Saturday are in the shared album 📸", 35, n = 8),
        inMsg("tp_1", "/maya/trip-planner", "Your train to Porto is booked: Fri 17:05, seat 42.", 95, n = 9),
        outMsg("pr_1", "/priya", "Coffee before the design review?", 190, MessageReceipt("seen", ago(190), ago(186)), 10),
        inMsg("pr_2", "/priya", "Sounds perfect, see you at 10!", 180, n = 11),
        inMsg("sam_1", "/sam", "Thanks for the intro 🙏", 1440, n = 12),
    )
    private val peers get() = messages.map { it.peerKey }.distinct()

    /**
     * The features one capture turns on (`--es pigeonpost.marketing.caps groups,avatars,...`). Each screenshot
     * asks for what its screen shows; with every feature on at once the group list did not appear here.
     */
    @Volatile var only: List<String>? = null
    override suspend fun capabilities() = only?.let { Capabilities(it) } ?: Capabilities(listOf(Features.GROUPS, Features.DISAPPEARING, Features.STREAMING, Features.MEETING_SUMMARIES,
        Features.AVATARS, "calendar", "orgs") + Features.MESSAGE_ACTIONS)
    override suspend fun identities() = listOf(IdentityRow(me, "Personal"))
    override suspend fun whoami(identity: String) = WhoAmI(identity, "/maya")
    override suspend fun inbox(identity: String, wait: Int?): InboxResponse {
        if (wait != null) delay(1000)
        return InboxResponse(messages.toList())
    }
    override suspend fun inboxWithActions(identity: String, wait: Int?) = inbox(identity, wait)
    override suspend fun filteredInboxWithActions(identity: String) = emptyList<Message>()
    override suspend fun filteredInbox(identity: String) = emptyList<Message>()
    override suspend fun threads(identity: String) = peers.map { p -> ServerThread("t_$p", p, isDefault = true, lastAt = messages.filter { it.peerKey == p }.maxOf { it.at }) }
    override suspend fun contacts(identity: String): ContactsResponse { ready.await(); return ContactsResponse(names.filterKeys { it in peers }.map { (h, n) ->
        if (h.startsWith("/maya/")) Contact(h, n, "allow", "auto", listOf("report_status", "answer_question", "read_file")) else Contact(h, n) } +
        // Group members by mailbox, so group previews name them before a group's member list is loaded; archived, so no empty rows.
        mailboxes.mapNotNull { (h, box) -> names[h]?.let { Contact(box, it) } },
        Vocabulary(listOf("report_status", "answer_question", "read_file", "run_tests"), listOf("run_shell", "git_push", "deploy", "read_credentials", "spend", "delete_files"))) }
    override suspend fun archive(identity: String): Set<String> { ready.await(); return mailboxes.values.toSet() }
    override suspend fun policy(identity: String) = FilterPolicy(readReceipts = true)
    override suspend fun ack(identity: String, messageId: String) {
        val i = messages.indexOfFirst { it.id == messageId }; if (i >= 0) messages[i] = messages[i].copy(read = true)
    }
    override suspend fun ackWithEdits(identity: String, messageId: String) = ack(identity, messageId)
    override suspend fun send(identity: String, to: String, body: String, threadId: String?, attachments: List<String>): SendResponse {
        val id = "mk_" + UUID.randomUUID()
        messages += Message(id, body, to = to, peer = to, peerHandle = to, direction = "out", sentAt = System.currentTimeMillis() / 1000, threadId = threadId ?: "t_$to",
            receipt = MessageReceipt("delivered", System.currentTimeMillis() / 1000))
        return SendResponse("received_$id", id)
    }
    override suspend fun sendWithActions(identity: String, to: String, body: String, threadId: String?, action: MessageSendAction, attachments: List<String>): SendResponse {
        val id = "mk_" + UUID.randomUUID()
        messages += Message(id, body, to = to, peer = to, peerHandle = to, direction = "out", sentAt = System.currentTimeMillis() / 1000, threadId = threadId ?: "t_$to",
            msgRef = action.msgRef, receipt = MessageReceipt("delivered", System.currentTimeMillis() / 1000))
        return SendResponse("received_$id", id, action.msgRef)
    }

    // ---- Avatars: a flat colour and an initial, drawn here; never a photo -------------------------
    override suspend fun avatars(keys: List<String>): Map<String, AvatarInfo?> = keys.associateWith { key ->
        val handle = fake.handles[key] ?: key
        colors[handle]?.let { AvatarInfo("av" + handle.replace('/', '_'), 1, "h" + handle.replace('/', '_'), "everyone",
            AvatarUrls(medium = "/v1/avatar-blobs/av${handle.replace('/', '_')}/1/128.png")) }
    }
    override suspend fun ownAvatar(identity: String) = avatars(listOf("/maya"))["/maya"]
    override suspend fun avatarBytes(path: String): ByteArray {
        val handle = colors.keys.firstOrNull { path.contains("av" + it.replace('/', '_') + "/") } ?: "/maya"
        val label = (names[handle] ?: "Maya")
        val agentLike = handle.startsWith("/maya/")
        val size = 256
        val bitmap = android.graphics.Bitmap.createBitmap(size, size, android.graphics.Bitmap.Config.ARGB_8888)
        val canvas = android.graphics.Canvas(bitmap)
        canvas.drawColor(colors.getValue(handle).toInt())
        val paint = android.graphics.Paint(android.graphics.Paint.ANTI_ALIAS_FLAG).apply {
            color = android.graphics.Color.WHITE; textAlign = android.graphics.Paint.Align.CENTER
            typeface = android.graphics.Typeface.create(android.graphics.Typeface.SANS_SERIF, android.graphics.Typeface.BOLD)
            textSize = if (agentLike) 96f else 120f
        }
        val text = if (agentLike) label.split(' ').joinToString("") { it.take(1).uppercase() } else label.take(1)
        canvas.drawText(text, size / 2f, size / 2f - (paint.descent() + paint.ascent()) / 2, paint)
        return java.io.ByteArrayOutputStream().also { bitmap.compress(android.graphics.Bitmap.CompressFormat.PNG, 100, it) }.toByteArray()
    }

    // ---- Groups: Weekend hikers (disappearing after a week) and Design team (live stream) ----------
    @Volatile var hikers: String? = null
    @Volatile var design: String? = null
    private val rooms = linkedMapOf<String, String>() // name -> group id
    private val roomCounts = mapOf("Launch room" to 9, "Design reviews" to 6, "Support desk" to 12)
    private val ready = CompletableDeferred<Unit>()
    private val clients = linkedMapOf<String, GroupClient>()
    private var hikersFirst = false
    private var hikersSecond = false
    private var designSeeded = false

    private fun times(gid: String, minutes: List<Int>) {
        val g = fake.groups[gid] ?: return
        var k = 0
        for (i in g.items.indices) {
            val item = g.items[i]
            if (item["type"]?.jsonPrimitive?.content != "application") continue
            val m = minutes.getOrNull(k++) ?: break
            g.items[i] = JsonObject(item + ("created_at" to JsonPrimitive(ago(m))))
        }
        g.lastAt = ago(minutes.getOrElse(k - 1) { 0 })
    }
    private fun applicationItems(gid: String) = fake.groups[gid]?.items.orEmpty().filter { it["type"]?.jsonPrimitive?.content == "application" }
    private fun active(gid: String?) = gid != null && fake.groups[gid]?.members?.get(me)?.state == "active"

    /** Marks the last [count] messages of Weekend hikers unread again (after the test has read and replied in it). */
    fun markHikersUnread(count: Int) {
        val g = fake.groups[hikers ?: return] ?: return
        val seqs = g.items.filter { it["type"]?.jsonPrimitive?.content == "application" }.mapNotNull { it["seq"]?.jsonPrimitive?.longOrNull }
        g.members[me]?.let { it.readCursor = seqs.getOrElse(seqs.size - count - 1) { 0 }; it.revision++ }
        g.revision++
    }

    fun run(scope: CoroutineScope) {
        scope.launch(Dispatchers.Default) {
            try {
                for (h in listOf("/leo", "/ana", "/kai", "/noor", "/priya", "/sam", "/tomas", agent)) {
                    val box = fake.mailbox(); fake.handles[box] = h; mailboxes[h] = box
                    val c = GroupClient(crypto, fake.transport, MemoryGroupStorage(), box, "mk" + h.replace('/', '-'), jitter = { 0 })
                    c.init(); c.topUpKeyPackages(); clients[h] = c
                }
                fun c(h: String) = clients.getValue(h)
                suspend fun build(owner: String, body: JsonObject, members: List<String>, invitesMe: Boolean): String {
                    val gid = c(owner).createGroup(body).group.id
                    for (m in members) { fake.invite(gid, mailboxes.getValue(m)); fake.accept(gid, mailboxes.getValue(m)) }
                    c(owner).processPending(gid, noJitter = true)
                    for (m in members) c(m).processWelcomes()
                    if (invitesMe) { fake.invite(gid, me); fake.accept(gid, me) }
                    return gid
                }
                hikers = build("/kai", buildJsonObject { put("name", "Weekend hikers"); put("description", "Trails, views and good coffee"); put("ephemeral_secs", 604800) },
                    listOf("/leo", "/ana", "/noor"), true)
                design = build("/priya", buildJsonObject { put("name", "Design team"); put("description", "Fernhill Studio design") },
                    listOf("/sam", "/tomas", "/leo", agent), true)
                for ((name, policy) in listOf("Launch room" to "public", "Design reviews" to "public", "Support desk" to "private")) {
                    val gid = c("/priya").createGroup(buildJsonObject { put("name", name); put("join_policy", policy)
                        put("description", when (name) { "Launch room" -> "Product launch: people and two agents"; "Design reviews" -> "Weekly critique and sign-off"; else -> "Customer questions, with /fernhill/support-agent" }) }).group.id
                    rooms[name] = gid
                }
                ready.complete(Unit)
                while (isActive) {
                    for ((_, x) in clients) try {
                        x.processWelcomes()
                        for ((gid, g) in fake.groups) {
                            if (g.members[x.mailbox] == null || !x.inGroup(gid)) continue
                            x.sync(gid); x.processPending(gid, noJitter = true)
                        }
                    } catch (cancelled: CancellationException) { throw cancelled } catch (e: Exception) { android.util.Log.e("PPMarketing", "loop", e) }
                    try {
                        val h = hikers!!; val d = design!!
                        if (!hikersFirst && active(h)) {
                            hikersFirst = true
                            c("/kai").send(h, OutgoingDocument("Trailhead at 8 on Saturday? Forecast says clear skies ☀️"))
                            c("/ana").send(h, OutgoingDocument("In! I'll bring the good coffee ☕"))
                            c("/leo").send(h, OutgoingDocument("Can we do the ridge loop this time? 14 km, great views."))
                            times(h, listOf(52, 47, 40))
                        }
                        if (hikersFirst && !hikersSecond && applicationItems(h).any { it["sender"]?.jsonPrimitive?.content == me }) {
                            hikersSecond = true
                            c("/noor").send(h, OutgoingDocument("Seat for me please 🙋"))
                            c("/kai").send(h, OutgoingDocument("Packed lunch at the summit, then the lake on the way down?"))
                            times(h, listOf(52, 47, 40, 33, 21, 8))
                        }
                        if (!designSeeded && active(d)) {
                            designSeeded = true
                            c("/tomas").send(d, OutgoingDocument("Morning! Slides for Friday are in the shared folder."))
                            c("/sam").send(d, OutgoingDocument("The new welcome step is so much clearer 👏"))
                            c(agent).send(d, OutgoingDocument("I summarized last week's feedback: 12 notes in 3 themes. Navigation, tone and empty states."))
                            c("/priya").send(d, OutgoingDocument("New onboarding screens are up for review 🎨 I'm going live with a walkthrough at 10."))
                            times(d, listOf(30, 24, 20, 14))
                            fake.groups[d]?.let { g -> g.members[me]?.let { it.readCursor = g.lastSeq; it.revision++ }; g.revision++ }
                        }
                    } catch (cancelled: CancellationException) { throw cancelled } catch (e: Exception) { android.util.Log.e("PPMarketing", "seed", e) }
                    delay(500)
                }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: Exception) { android.util.Log.e("PPMarketing", "world", failure); ready.completeExceptionally(failure) }
        }
    }

    // ---- Streams and meeting notes ----------------------------------------------------------------
    private val streamId = "str_marketingfridaydemoaaaaaaa"
    private fun stream(gid: String) = buildJsonObject {
        put("id", streamId); put("group_id", gid); put("title", "Friday demo: new onboarding"); putJsonArray("sources") { add("camera"); add("screen") }
        put("by", mailboxes["/priya"] ?: ""); put("by_handle", "/priya"); put("state", "live"); put("started_at", ago(6))
        put("viewers", 48); put("revision", 1); putJsonArray("stage") {}
    }
    private val launchSync = "mtg_launchsync0000000000000001"
    private val designReview = "mtg_designreview00000000000002"
    private val registered = java.util.concurrent.ConcurrentHashMap<String, String>()
    private fun brief(id: String) = if (id == launchSync) buildJsonObject {
        put("v", 1); put("meeting_id", id); put("title", "Launch sync"); put("language", "en")
        put("started_at", Instant.ofEpochSecond(ago(90)).toString()); put("duration_s", 42 * 60)
        putJsonArray("participants") { listOf("/maya", "/leo", "/ana", "/priya", agent).forEach { add(it) } }
        put("summary", "The team agreed to launch on May 20 with the new onboarding flow. Harbor Hall is held for the May offsite.")
        putJsonArray("decisions") { add("Launch date: Tuesday, May 20"); add("Ship the new onboarding screens in 1.2"); add("Offsite at Harbor Hall, May 14–15") }
        putJsonArray("action_items") {
            add(buildJsonObject { put("owner", "/priya"); put("task", "Final onboarding designs"); put("due", "Friday") })
            add(buildJsonObject { put("owner", "/leo"); put("task", "Confirm the Harbor Hall booking"); put("due", "Thursday") })
            add(buildJsonObject { put("owner", agent); put("task", "Draft the launch announcement"); put("due", "Monday") })
        }
        putJsonArray("open_questions") { add("Do we need a press preview the week before?") }
        put("draft", false)
        putJsonArray("transcript") {
            add(buildJsonObject { put("at", "00:12"); put("speaker", "/maya"); put("text", "Let's lock the launch date first.") })
            add(buildJsonObject { put("at", "03:40"); put("speaker", "/priya"); put("text", "Onboarding designs will be final by Friday.") })
            add(buildJsonObject { put("at", "11:05"); put("speaker", "/leo"); put("text", "I'll confirm Harbor Hall for the offsite.") })
        }
        putJsonObject("generated_by") { put("stt", "whisper.cpp ggml-small"); put("llm", "local model") }
    } else buildJsonObject {
        put("v", 1); put("meeting_id", id); put("title", "Design review"); put("language", "en")
        put("started_at", Instant.ofEpochSecond(ago(26 * 60)).toString()); put("duration_s", 31 * 60)
        putJsonArray("participants") { listOf("/maya", "/priya", "/sam").forEach { add(it) } }
        put("summary", "Reviewed the new onboarding screens; the welcome step is approved with two copy changes.")
        putJsonArray("decisions") { add("Approve the welcome step") }
        putJsonArray("action_items") { add(buildJsonObject { put("owner", "/sam"); put("task", "Update the empty-state copy"); put("due", "Wednesday") }) }
        putJsonArray("open_questions") {}
        put("draft", false)
        putJsonArray("transcript") {}
        putJsonObject("generated_by") { put("stt", "whisper.cpp ggml-small"); put("llm", "local model") }
    }
    private fun meetingRow(id: String, created: Long) = buildJsonObject {
        put("id", id); put("kind", "call"); put("state", "ready"); put("created_at", created); put("expires_at", created + 90 * 86400); put("host", "/maya")
    }

    // ---- GroupTransport: groups, rooms, streams and meeting notes ----------------------------------
    override suspend fun groupJson(mailbox: String, method: String, path: String, body: JsonElement?, device: String?): String {
        ready.await()
        val route = path.substringBefore('?')
        val d = design
        when {
            route == "/v1/orgs" -> return WireJson.encodeToString(RoomOrganizations.serializer(), RoomOrganizations(listOf(RoomOrganization("org_fernhill", "Fernhill Studio", "active",
                "member_maya", namespace = "fernhill", settings = RoomOrganizationSettings("everyone"), mailbox = RoomMailbox(me, "/maya")))))
            route == "/v1/rooms" -> return buildJsonObject { putJsonArray("rooms") {
                for ((name, id) in rooms) { val g = fake.groups[id] ?: continue; add(buildJsonObject {
                    put("id", id); put("name", name); put("description", g.description); put("join_policy", g.joinPolicy)
                    put("member_count", roomCounts[name] ?: 3); g.members[mailbox]?.let { put("my_state", it.state) } }) }
            } }.toString()
            route == "/v1/groups/resolve" -> {
                val id = (body as? JsonObject)?.get("group_id")?.jsonPrimitive?.content
                val name = rooms.entries.firstOrNull { it.value == id }?.key ?: throw ApiException(404, "not_found")
                val g = fake.groups.getValue(id!!)
                return buildJsonObject { put("id", id); put("name", name); put("description", g.description); put("member_count", roomCounts[name] ?: 3)
                    put("join_policy", g.joinPolicy); put("can_join", g.joinPolicy == "public") }.toString()
            }
            method == "GET" && d != null && route == "/v1/groups/$d/streams" -> return buildJsonObject { putJsonArray("streams") { add(stream(d)) } }.toString()
            method == "GET" && Regex("/v1/groups/grp_[a-z2-7]+/streams").matches(route) -> return """{"streams":[]}"""
            method == "GET" && d != null && route == "/v1/streams/$streamId" -> return buildJsonObject { put("stream", stream(d)); put("role", "viewer") }.toString()
            route == "/v1/meetings" && method == "GET" -> return buildJsonObject { putJsonArray("meetings") {
                add(meetingRow(launchSync, ago(48))); add(meetingRow(designReview, ago(26 * 60))) } }.toString()
            route.startsWith("/v1/meetings/keys/") && method == "PUT" -> {
                val id = route.substringAfterLast('/'); registered[id] = (body as JsonObject)["public_key"]!!.jsonPrimitive.content
                return """{"key_id":"$id","registered":true}"""
            }
            route.startsWith("/v1/meetings/mtg_") && method == "GET" -> {
                val id = route.substringAfterLast('/')
                val created = if (id == launchSync) ago(48) else ago(26 * 60)
                val sealed = MeetingNotesCrypto.seal(id, brief(id).toString().toByteArray(), registered.map { (k, v) -> k to MeetingNotesCrypto.unb64(v) })
                return JsonObject(meetingRow(id, created) + mapOf("alg" to JsonPrimitive("A256GCM"), "iv" to JsonPrimitive(sealed.iv), "ciphertext" to JsonPrimitive(sealed.ciphertext),
                    "keys" to JsonArray(sealed.wraps.map { w -> buildJsonObject { put("key_id", w.keyId); put("epk", w.epk); put("iv", w.iv); put("wrapped", w.wrapped) } }))).toString()
            }
        }
        val response = fake.transport.groupJson(mailbox, method, path, body, device)
        if (method == "GET" && d != null && (route == "/v1/groups" || route == "/v1/groups/$d")) {
            val parsed = WireJson.parseToJsonElement(response).jsonObject
            fun withPolicy(g: JsonElement): JsonElement = if (g.jsonObject["id"]?.jsonPrimitive?.content == d) JsonObject(g.jsonObject + ("stream_policy" to JsonPrimitive("everyone"))) else g
            return when {
                parsed["groups"] != null -> JsonObject(parsed + ("groups" to JsonArray(parsed["groups"]!!.jsonArray.map(::withPolicy))))
                parsed["id"] != null -> withPolicy(parsed)
                else -> parsed
            }.toString()
        }
        return response
    }
    override suspend fun groupUpload(mailbox: String, path: String, bytes: ByteArray, device: String) = fake.transport.groupUpload(mailbox, path, bytes, device)
    override suspend fun groupDownload(mailbox: String, path: String, device: String?, maximumBytes: Long) = fake.transport.groupDownload(mailbox, path, device, maximumBytes)

    // ---- Calendar: the capture week --------------------------------------------------------------
    private val zone = ZoneId.systemDefault().id
    private val today = LocalDate.now()
    private val events = linkedMapOf<String, ScheduledEvent>()
    private fun person(h: String) = CalendarPerson(handle = h)
    private fun event(id: String, title: String, dayOffset: Long, start: String, end: String, with: List<String>, meeting: Boolean, location: String? = null,
                      rsvp: String? = null, organizer: String = "/maya", calendar: String = "cal_work"): ScheduledEvent {
        val day = today.plusDays(dayOffset).toString()
        val s = CalendarWallTime(day + "T" + start, zone).resolve(); val e = CalendarWallTime(day + "T" + end, zone).resolve()
        val mine = organizer == "/maya"
        return ScheduledEvent(id, revision = 1, title = title, location = location, allDay = false, start = day + "T" + start, end = day + "T" + end, tzid = zone,
            startsAt = s, endsAt = e, canEdit = mine, calendarId = calendar, organizer = person(organizer),
            my = CalendarMembership(role = if (mine) "organizer" else "attendee", rsvp = rsvp ?: if (mine) null else "accepted"),
            reminders = listOf(CalendarReminder(10, "push")),
            attendees = with.mapIndexed { i, h -> CalendarAttendee("att_${id}_$i", kind = "member", handle = h, name = names[h], role = "attendee", rsvp = "accepted") },
            meeting = if (meeting) ScheduledMeeting(enabled = true, code = "MKTG" + id.takeLast(4).uppercase(), opensAt = (s ?: 0) - 600, closesAt = (e ?: 0) + 600) else null)
    }
    init {
        listOf(
            event("evt_offsite", "Offsite planning", -1, "11:00", "11:30", listOf("/priya", "/maya/research-agent"), true, organizer = "/priya"),
            event("evt_standup", "Team standup", 0, "09:00", "09:15", listOf("/priya", "/sam", "/leo"), true),
            event("evt_launch", "Launch sync", 0, "10:00", "10:45", listOf("/leo", "/ana", "/priya", "/maya/research-agent"), true),
            event("evt_lunch", "Lunch with Ana 🥗", 0, "12:30", "13:30", listOf("/ana"), false, "Corner café", calendar = "cal_personal"),
            event("evt_design", "Design review", 0, "14:00", "15:00", listOf("/priya", "/sam"), true, organizer = "/priya"),
            event("evt_ramen", "Ramen with Leo 🍜", 0, "19:30", "21:00", listOf("/leo"), false, "Kōji Ramen", calendar = "cal_personal"),
            event("evt_one", "1:1 with Leo", 1, "09:30", "10:00", listOf("/leo"), true),
            event("evt_hike", "Weekend hike ⛰️", 3, "08:00", "14:00", listOf("/kai", "/ana", "/leo", "/noor"), false, "Ridge loop trailhead", calendar = "cal_personal"),
            event("evt_invite", "Roadmap brainstorm", 5, "11:00", "11:30", listOf("/priya", "/sam"), true, rsvp = "pending", organizer = "/priya"),
        ).forEach { events[it.id] = it }
    }
    private val folders = listOf(CalendarFolder("cal_work", "Work", "blue", true, 1), CalendarFolder("cal_personal", "Personal", "green", false, 1))
    private fun eventBytes(e: ScheduledEvent, occurrence: String? = null) = WireJson.encodeToString(CalendarEventResponse.serializer(), CalendarEventResponse(e,
        occurrence?.let { EventOccurrenceDetail(occurrenceId = it, start = e.start, end = e.end, startsAt = e.startsAt, endsAt = e.endsAt, title = e.title, myRsvp = e.my?.rsvp) })).toByteArray()
    override suspend fun scheduledMeeting(mailbox: String, code: String) = CalendarMeetingStatus("unavailable", emptyList())
    override suspend fun calendarRequest(mailbox: String, method: String, path: String, query: Map<String, String>, body: ByteArray?, mediaType: String): ByteArray {
        when (path) {
            "/v1/calendars" -> return WireJson.encodeToString(CalendarFoldersResponse.serializer(), CalendarFoldersResponse(folders)).toByteArray()
            "/v1/calendar/settings" -> return WireJson.encodeToString(CalendarSettings.serializer(), CalendarSettings(timezone = zone, weekStart = 1, previews = true)).toByteArray()
            "/v1/calendar/invitations" -> return if (query["state"] == "filtered") """{"invitations":[]}""".toByteArray() else
                """{"invitations":[{"event_id":"evt_invite","occurrence_id":"occ_evt_invite","title":"Roadmap brainstorm","organizer":{"handle":"/priya"}}]}""".toByteArray()
            "/v1/calendar/events" -> return WireJson.encodeToString(EventOccurrencesResponse.serializer(), EventOccurrencesResponse(events.values.map { e ->
                EventOccurrence("occ_" + e.id, e.id, calendarId = e.calendarId, allDay = e.allDay, startsAt = e.startsAt, endsAt = e.endsAt, startDate = e.startDate, endDate = e.endDate,
                    tzid = e.tzid, title = e.title, location = e.location, status = e.status, organizer = e.organizer, role = e.my?.role, myRsvp = e.my?.rsvp, meeting = e.meeting) })).toByteArray()
        }
        if (path.startsWith("/v1/calendar/events/")) {
            val rest = path.removePrefix("/v1/calendar/events/"); val id = rest.substringBefore('/')
            val e = events[id] ?: throw ApiException(404, "not_found")
            if (method == "GET") return eventBytes(e, query["occurrence_id"])
            return "{}".toByteArray()
        }
        if (path.startsWith("/v1/calendar/feeds/")) return """{"enabled":false}""".toByteArray()
        throw ApiException(404, "not_found")
    }
}
