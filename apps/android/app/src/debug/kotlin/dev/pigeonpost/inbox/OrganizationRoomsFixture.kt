package dev.pigeonpost.inbox

import dev.pigeonpost.core.*
import dev.pigeonpost.core.enterprise.*
import dev.pigeonpost.core.groups.*
import kotlinx.coroutines.*
import kotlinx.serialization.json.*
import java.io.IOException
import java.util.concurrent.ConcurrentHashMap
import java.util.concurrent.atomic.AtomicInteger

/** Opt-in native room fixtures. Real MLS, synthetic people and in-memory transport only. */
internal class OrganizationRoomsFixture private constructor(val mode: String, val session: EnterpriseFixtureSession,
    val fake: FakeGroupPostbox, val me: String, private val bob: String, private val base: PostboxApi) : PostboxApi by base, GroupTransport {
    companion object {
        @Volatile var current: OrganizationRoomsFixture? = null
        fun create(mode: String, postbox: (String) -> PostboxApi): OrganizationRoomsFixture {
            val fake = FakeGroupPostbox().apply { serialize = true }
            val me = fake.mailbox(); val bob = fake.mailbox()
            fake.handles[me] = "/demo/main"; fake.handles[bob] = "/bob/main"
            return OrganizationRoomsFixture(mode, EnterpriseFixtureSession(mode), fake, me, bob, postbox(me)).also { current = it }
        }
    }
    val crypto = UniffiGroupCrypto()
    val catalogGets = AtomicInteger(); val directoryGets = AtomicInteger(); val creates = AtomicInteger(); val joins = AtomicInteger(); val accepts = AtomicInteger(); val groupReads = AtomicInteger()
    val roomIds = ConcurrentHashMap<String, String>()
    private val registered = ConcurrentHashMap.newKeySet<String>()
    private val creationReplies = ConcurrentHashMap<String, Pair<String, String>>()
    val creationBodies = java.util.Collections.synchronizedList(mutableListOf<String>())
    private val ready = CompletableDeferred<Unit>()
    override suspend fun capabilities(): Capabilities { ready.await(); return Capabilities(if (mode == "org-rooms-off") listOf("groups") else listOf("groups", "orgs")) }
    fun run(scope: CoroutineScope) {
        scope.launch(Dispatchers.Default) {
            try {
                val owner = GroupClient(crypto, fake.transport, MemoryGroupStorage(), bob, "fixture-bob", jitter = { 0 })
                owner.init(); owner.topUpKeyPackages()
                for ((name, policy) in listOf("Design room" to "public", "Finance room" to "private", "Private room" to "private")) {
                    val group = owner.createGroup(buildJsonObject { put("name", name); put("join_policy", policy); put("description", "An encrypted team room") }).group
                    roomIds[name] = group.id; registered += group.id
                    if (name == "Finance room") fake.invite(group.id, me)
                }
                ready.complete(Unit)
                while (isActive) {
                    for (id in registered) try { if (owner.inGroup(id)) { owner.sync(id); owner.processPending(id, noJitter = true) } }
                    catch (cancelled: CancellationException) { throw cancelled } catch (_: Exception) {}
                    delay(250)
                }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: Exception) { ready.completeExceptionally(failure) }
        }
    }
    private suspend fun authorized() {
        val child = currentCoroutineContext()[AuthorizationContext]?.lease ?: throw AuthorizationRetired()
        child.requireActive(); val root = session.grant ?: throw AuthorizationRetired()
        if (!child.sharesAuthorization(root)) throw AuthorizationRetired()
    }
    private fun patch(group: JsonObject): JsonObject {
        val id = group["id"]?.jsonPrimitive?.content ?: return group
        return if (id in registered) JsonObject(group + ("org_id" to JsonPrimitive("org_fixture"))) else group
    }
    override suspend fun groupJson(mailbox: String, method: String, path: String, body: JsonElement?, device: String?): String {
        authorized(); ready.await(); authorized()
        val route = path.substringBefore('?'); val input = body as? JsonObject ?: buildJsonObject {}
        if (route == "/v1/orgs") {
            catalogGets.incrementAndGet()
            val catalog = RoomOrganizations(if (mode == "org-rooms-no-membership") emptyList() else listOf(RoomOrganization("org_fixture", "Example", if (mode == "org-rooms-readonly") "read_only" else "active", "member_fixture",
                namespace = "demo", settings = RoomOrganizationSettings("everyone"), mailbox = RoomMailbox(me, "/demo/main"))))
            return WireJson.encodeToString(RoomOrganizations.serializer(), catalog)
        }
        if (route == "/v1/rooms") {
            directoryGets.incrementAndGet()
            if (mode == "org-rooms-no-membership") throw ApiException(403, "not_org_member")
            val query = java.net.URI("http://fixture.invalid$path").rawQuery.orEmpty().split('&').associate {
                it.substringBefore('=') to java.net.URLDecoder.decode(it.substringAfter('=', ""), "UTF-8")
            }
            if (query["org_id"] != "org_fixture") throw ApiException(400, "invalid_org")
            val rooms = registered.mapNotNull { id -> fake.groups[id]?.takeIf { it.deleted == null }?.let { group -> buildJsonObject {
                put("id", id); put("name", group.name); put("description", group.description); put("join_policy", group.joinPolicy)
                put("member_count", group.members.values.count { it.state == "active" }); group.members[mailbox]?.let { put("my_state", it.state) }
            } } }.filter { it.getValue("name").jsonPrimitive.content.contains(query["q"].orEmpty(), ignoreCase = true) }.sortedBy { it.getValue("name").jsonPrimitive.content }
            return buildJsonObject { put("rooms", JsonArray(rooms)) }.toString()
        }
        if (route == "/v1/groups/resolve") {
            val id = input["group_id"]?.jsonPrimitive?.content ?: throw ApiException(404, "not_found")
            val group = fake.groups[id]?.takeIf { id in registered } ?: throw ApiException(404, "not_found")
            val mine = group.members[mailbox]?.state
            return buildJsonObject {
                put("id", id); put("name", group.name); put("description", group.description); put("member_count", group.members.values.count { it.state == "active" })
                put("join_policy", group.joinPolicy); mine?.let { put("my_state", it) }
                put("can_join", group.joinPolicy == "public" && mine !in setOf("active", "keying", "invited"))
            }.toString()
        }
        if (route.startsWith("/v1/groups/") && route.endsWith("/join") && method == "POST") {
            joins.incrementAndGet(); val id = route.removePrefix("/v1/groups/").removeSuffix("/join")
            val group = fake.groups[id]?.takeIf { id in registered } ?: throw ApiException(404, "not_found")
            if (group.joinPolicy != "public") throw ApiException(403, "invite_required")
            if (group.members[mailbox]?.state in setOf("active", "keying")) throw ApiException(409, "already_member")
            fake.invite(id, mailbox); fake.accept(id, mailbox); return "{}"
        }
        if (route.endsWith("/invitation") && method == "POST") accepts.incrementAndGet()
        if (route == "/v1/groups" && method == "POST") {
            creates.incrementAndGet(); val encoded = input.toString(); creationBodies += encoded
            val key = input["request_key"]?.jsonPrimitive?.content ?: throw ApiException(400, "missing_request_key")
            creationReplies[key]?.let { (original, reply) -> if (original != encoded) throw ApiException(409, "idempotency_conflict"); return reply }
            val result = WireJson.parseToJsonElement(fake.transport.groupJson(mailbox, method, path, input, device)).jsonObject
            val id = result.getValue("group").jsonObject.getValue("id").jsonPrimitive.content; registered += id
            // The common fake returns only a creation ID; the real route returns fresh_view,
            // including creator and explicit owner/keying membership. Preserve that full receipt.
            val group = fake.groupJson(fake.groups.getValue(id), mailbox)
            val reply = JsonObject(result + ("group" to patch(group))).toString(); creationReplies[key] = encoded to reply
            if (mode == "org-rooms-lost" && creates.get() == 1) throw IOException("Fixture accepted response loss")
            return reply
        }
        if (Regex("/v1/groups/[^/]+").matches(route) && method == "GET") {
            val count = groupReads.incrementAndGet()
            if (mode == "org-rooms-revoked" && count >= 2) throw ApiException(403, "not_org_member")
            val raw = WireJson.parseToJsonElement(fake.transport.groupJson(mailbox, method, path, body, device)).jsonObject
            return patch(raw).toString()
        }
        val result = fake.transport.groupJson(mailbox, method, path, body, device)
        if (route == "/v1/groups" && method == "GET") {
            val raw = WireJson.parseToJsonElement(result).jsonObject
            return JsonObject(raw + ("groups" to JsonArray(raw["groups"]!!.jsonArray.map { patch(it.jsonObject) }))).toString()
        }
        return result
    }
    override suspend fun groupUpload(mailbox: String, path: String, bytes: ByteArray, device: String): String { authorized(); return fake.transport.groupUpload(mailbox, path, bytes, device).also { authorized() } }
    override suspend fun groupDownload(mailbox: String, path: String, device: String?, maximumBytes: Long): ByteArray { authorized(); return fake.transport.groupDownload(mailbox, path, device, maximumBytes).also { authorized() } }
}
