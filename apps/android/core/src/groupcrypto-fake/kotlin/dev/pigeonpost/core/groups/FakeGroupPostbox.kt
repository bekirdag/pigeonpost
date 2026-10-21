package dev.pigeonpost.core.groups

import dev.pigeonpost.core.ApiException
import dev.pigeonpost.groupcrypto.inspectEnvelope
import dev.pigeonpost.groupcrypto.inspectKeyPackage
import dev.pigeonpost.groupcrypto.mailboxForSeed
import dev.pigeonpost.groupcrypto.signAttestationWithSeed
import dev.pigeonpost.groupcrypto.verifyEnvelopeSignature
import dev.pigeonpost.groupcrypto.welcomeRecipients
import kotlinx.serialization.json.*
import kotlinx.coroutines.sync.withLock
import java.security.SecureRandom
import java.util.Base64

/**
 * An in-memory postbox implementing the group and MLS rules of docs/groups.md §1–§3, using the real
 * library's server helpers (inspectKeyPackage, inspectEnvelope, verifyEnvelopeSignature,
 * welcomeRecipients) for the checks the postbox makes with the Rust crate. A port of the web
 * inbox's site-inbox/test/fake-groups-postbox.mjs: the contract the client is tested against.
 */
class FakeGroupPostbox {
    private fun b64(b: ByteArray) = Base64.getEncoder().encodeToString(b)
    private fun unb64(s: String) = Base64.getDecoder().decode(s)
    private fun hex(b: ByteArray) = b.joinToString("") { "%02x".format(it) }

    class Device(val sigKey: ByteArray, var revoked: Boolean = false)
    class Kp(val mailbox: String, val deviceId: String, val bytes: String, val lastResort: Boolean, val ref: String, var claimedBy: String? = null)
    class Member(var role: String, var state: String, var firstSeq: Long? = null, var invitedBy: String? = null) {
        var revision = 1L; var notify = "all"; var mutedUntil: Long? = null; var readCursor = 0L
    }
    class G(val id: String, var name: String, val creator: String) {
        var revision = 1L; var epoch = 0L; var activation = "provisioning"; var lastSeq = 0L
        var postPolicy = "everyone"; var ephemeral = 0L
        var description: String? = null; var joinPolicy = "private"; var membersCanInvite = false; var membersCanChangeTimer = false
        var linkToken: String? = null; var deleted: Long? = null; var lastAt = 0L
        val items = mutableListOf<JsonObject>()
        val members = linkedMapOf<String, Member>()
        val leaves = mutableSetOf<String>()
        val stale = mutableSetOf<String>()
        val claims = mutableMapOf<String, Triple<String, String, String>>() // ref -> (by, mailbox, device)
    }
    class W(val id: String, val groupId: String, val epoch: Long, val seq: Long, val welcome: String, val mailbox: String, val deviceId: String, var acked: Boolean = false)

    val seeds = mutableMapOf<String, ByteArray>()
    val publicKeys = mutableMapOf<String, ByteArray>()
    val devices = mutableMapOf<String, MutableMap<String, Device>>()
    val kps = linkedMapOf<String, Kp>()
    val groups = mutableMapOf<String, G>()
    val welcomes = mutableListOf<W>()
    val blobs = mutableMapOf<String, ByteArray>()
    val calls = mutableListOf<Triple<String, String, String>>()
    /** Display handles for mailboxes, as the members route reports them. */
    val handles = mutableMapOf<String, String>()
    /** Run every request one at a time (the app's fixtures call in from several coroutines and threads). */
    var serialize = false
    private val serial = kotlinx.coroutines.sync.Mutex()
    val errors = mutableListOf<Pair<String, String>>()
    var beforeHandle: (suspend (String, String, String) -> Unit)? = null
    private var next = 1
    private fun id(p: String) = "${p}_${next++}"
    private fun now() = System.currentTimeMillis() / 1000

    /** A self-custodied mailbox: its /k/ address and ed25519 key come from a random seed. */
    fun mailbox(): String {
        val seed = ByteArray(32).also(SecureRandom()::nextBytes)
        val address = mailboxForSeed(seed)
        seeds[address] = seed
        publicKeys[address] = edPublic(seed)
        devices[address] = mutableMapOf()
        return address
    }

    /** RFC 8032 §5.1.5 public key for a seed. Plain arithmetic: Android's JCA has no Ed25519, and this is test code. */
    private fun edPublic(seed: ByteArray): ByteArray = Ed25519Public.fromSeed(seed)

    private fun attestationPayload(mailbox: String, deviceId: String, sigKey: ByteArray): ByteArray {
        val out = java.io.ByteArrayOutputStream()
        fun u16(n: Int) { out.write((n shr 8) and 255); out.write(n and 255) }
        out.write("pigeonpost/groupcrypto/device-attestation/v1\u0000".toByteArray())
        val m = mailbox.toByteArray(); val d = deviceId.toByteArray()
        u16(m.size); out.write(m); u16(d.size); out.write(d); u16(1); u16(sigKey.size); out.write(sigKey)
        return out.toByteArray()
    }

    private fun key(m: String, d: String) = "$m|$d"
    private fun dev(me: String, device: String?): Device {
        val d = devices[me]?.get(device) ?: throw ApiException(403, "unknown_device")
        if (d.revoked) throw ApiException(403, "device_revoked")
        return d
    }
    private fun group(gid: String) = groups[gid] ?: throw ApiException(404, "not_found")

    fun pending(g: G): List<JsonObject> {
        val out = mutableListOf<JsonObject>()
        for ((m, mem) in g.members) {
            if (mem.state != "active" && mem.state != "keying") continue
            if (g.activation == "provisioning" && m != g.creator) continue
            for ((d, dv) in devices[m].orEmpty()) {
                if (dv.revoked) continue
                if (key(m, d) !in g.leaves) out += buildJsonObject { put("kind", "add"); put("mailbox", m); put("device_id", d); put("since", 0) }
            }
        }
        for (k in g.leaves) {
            val (m, d) = k.split("|")
            val mem = g.members[m]; val dv = devices[m]?.get(d)
            if (mem == null || mem.state !in setOf("active", "keying") || dv == null || dv.revoked || k in g.stale)
                out += buildJsonObject { put("kind", "remove"); put("mailbox", m); put("device_id", d); put("since", 0) }
        }
        return out
    }

    private fun push(g: G, item: JsonObject): JsonObject {
        g.lastSeq += 1
        g.lastAt = now()
        val full = JsonObject(item + mapOf("seq" to JsonPrimitive(g.lastSeq), "created_at" to JsonPrimitive(now())))
        g.items += full
        return full
    }

    // Server-side actions the tests take directly (the member routes are covered by GroupStore).
    fun invite(gid: String, address: String) { val g = group(gid); g.members[address] = Member("member", "invited"); push(g, buildJsonObject { put("type", "notice"); put("kind", "member_invited"); putJsonObject("metadata") { put("address", address) } }) }
    fun accept(gid: String, address: String) { val m = group(gid).members[address] ?: throw ApiException(403, "invite_required"); m.state = "keying" }
    fun remove(gid: String, address: String) { val g = group(gid); g.members.getValue(address).state = "removed"; g.revision++; push(g, buildJsonObject { put("type", "notice"); put("kind", "member_removed"); putJsonObject("metadata") { put("address", address) } }) }

    val transport = object : GroupTransport {
        override suspend fun groupJson(mailbox: String, method: String, path: String, body: JsonElement?, device: String?): String = record(mailbox, path) {
            handle(mailbox, method, path, body?.jsonObject ?: JsonObject(emptyMap()), device).toString()
        }
        override suspend fun groupUpload(mailbox: String, path: String, bytes: ByteArray, device: String): String = record(mailbox, path) {
            dev(mailbox, device)
            val att = id("gat"); blobs[att] = bytes
            buildJsonObject { put("id", att); put("bytes", bytes.size) }.toString()
        }
        override suspend fun groupDownload(mailbox: String, path: String, device: String?, maximumBytes: Long): ByteArray = record(mailbox, path) {
            val m = Regex("^/v1/groups/([^/]+)/attachments/(.+)$").find(path) ?: throw ApiException(404, "not_found")
            val g = group(m.groupValues[1])
            if (g.members[mailbox]?.state != "active") throw ApiException(403, "not_member")
            blobs[java.net.URLDecoder.decode(m.groupValues[2], "UTF-8")] ?: throw ApiException(404, "not_found")
        }
    }

    private suspend fun <T> record(me: String, path: String, work: suspend () -> T): T = try {
        if (serialize) serial.withLock { work() } else work()
    } catch (e: ApiException) { errors += me to (e.code ?: ""); throw e }

    private fun unread(g: G, me: String): Int {
        val m = g.members[me] ?: return 0
        return g.items.count { it.str("type") == "application" && it.long("seq") > m.readCursor && it.str("sender") != me && it.long("seq") >= (m.firstSeq ?: 0) }
    }
    fun groupJson(g: G, me: String): JsonObject {
        val m = g.members[me]
        val invitedOnly = m?.state == "invited"
        return buildJsonObject {
            put("id", g.id); put("name", g.name); g.description?.let { put("description", it) }; put("join_policy", g.joinPolicy)
            put("member_count", g.members.values.count { it.state == "active" })
            if (!invitedOnly) {
                put("post_policy", g.postPolicy); put("visibility", "unlisted"); put("members_can_invite", g.membersCanInvite)
                put("members_can_change_timer", g.membersCanChangeTimer); put("ephemeral_secs", if (g.ephemeral > 0) JsonPrimitive(g.ephemeral) else JsonNull)
                put("revision", g.revision); put("epoch", g.epoch); put("activation_state", g.activation); put("created_by", g.creator)
                put("last_at", g.lastAt); put("last_seq", g.lastSeq); put("has_link", g.linkToken != null)
                g.deleted?.let { put("deleted_at", it) }
            }
            if (m != null) putJsonObject("my") {
                put("role", m.role); put("state", m.state); put("read_cursor", m.readCursor); put("unread", unread(g, me)); put("notify", m.notify)
                put("muted_until", m.mutedUntil?.let { JsonPrimitive(it) } ?: JsonNull); m.invitedBy?.let { put("invited_by", it) }; put("revision", m.revision)
            }
        }
    }
    private fun notice(g: G, kind: String, actor: String, meta: JsonObject = JsonObject(emptyMap())) =
        push(g, buildJsonObject { put("type", "notice"); put("kind", kind); put("actor", actor); put("metadata", meta) })
    private fun admin(g: G, me: String) = g.members[me]?.role in setOf("owner", "admin") && g.members[me]?.state == "active"

    /** Group management routes (docs/groups.md §2), enough for the app's screens. */
    private fun manage(me: String, method: String, path: String, query: Map<String, String>, body: JsonObject): JsonElement? {
        fun s(k: String) = (body[k] as? JsonPrimitive)?.contentOrNull
        if (path == "/v1/groups" && method == "GET") {
            val states = (query["state"]?.let { java.net.URLDecoder.decode(it, "UTF-8") } ?: "active,invited,keying").split(',')
            return buildJsonObject { put("groups", JsonArray(groups.values.filter { g -> g.members[me]?.state in states || (g.deleted != null && g.members[me] != null) }
                .sortedByDescending { it.lastAt }.map { groupJson(it, me) })) }
        }
        if (path == "/v1/groups/resolve") {
            val g = groups[s("group_id")] ?: throw ApiException(404, "not_found")
            val token = s("link_token")
            val mine = g.members[me]?.state
            val viaLink = token != null && token == g.linkToken
            if (mine == null && !viaLink) throw ApiException(404, "not_found")
            return buildJsonObject { put("id", g.id); put("name", g.name); g.description?.let { put("description", it) }
                put("member_count", g.members.values.count { it.state == "active" }); put("join_policy", g.joinPolicy); put("visibility", "unlisted")
                mine?.let { put("my_state", it) }; put("can_join", viaLink && g.joinPolicy == "public" && mine !in setOf("active", "keying")); put("needs", if (mine == "invited") "invitation" else "link") }
        }
        val m = Regex("^/v1/groups/([^/]+)(/.*)?$").find(path) ?: return null
        val g = groups[java.net.URLDecoder.decode(m.groupValues[1], "UTF-8")] ?: throw ApiException(404, "not_found")
        val rest = m.groupValues[2]
        val mine = g.members[me]
        when {
            rest == "" && method == "GET" -> { if (mine == null) throw ApiException(404, "not_found"); return groupJson(g, me) }
            rest == "" && method == "PATCH" -> {
                val timerOnly = body.keys.all { it in setOf("ephemeral_secs", "expected_revision") }
                if (!admin(g, me) && !(timerOnly && g.membersCanChangeTimer && mine?.state == "active")) throw ApiException(403, "not_admin")
                if (body.long("expected_revision") != g.revision) throw ApiException(409, "revision_conflict", problem = buildJsonObject { put("current_revision", g.revision) })
                val changed = mutableListOf<String>()
                s("name")?.let { g.name = it; changed += "name" }
                if ("description" in body) { g.description = s("description"); changed += "description" }
                s("join_policy")?.let { g.joinPolicy = it; changed += "join_policy" }
                s("post_policy")?.let { g.postPolicy = it; changed += "post_policy" }
                (body["members_can_invite"] as? JsonPrimitive)?.booleanOrNull?.let { g.membersCanInvite = it; changed += "members_can_invite" }
                (body["members_can_change_timer"] as? JsonPrimitive)?.booleanOrNull?.let { g.membersCanChangeTimer = it; changed += "members_can_change_timer" }
                if ("ephemeral_secs" in body) {
                    g.ephemeral = (body["ephemeral_secs"] as? JsonPrimitive)?.longOrNull ?: 0
                    notice(g, "timer_changed", me, buildJsonObject { put("ephemeral_secs", if (g.ephemeral > 0) JsonPrimitive(g.ephemeral) else JsonNull) })
                }
                g.revision++
                if (changed.isNotEmpty()) notice(g, "settings_changed", me, buildJsonObject { put("changed", JsonArray(changed.map(::JsonPrimitive))); put("post_policy", g.postPolicy); put("name", g.name); put("join_policy", g.joinPolicy) })
                return buildJsonObject { put("group", groupJson(g, me)) }
            }
            rest == "" && method == "DELETE" -> { if (mine?.role != "owner") throw ApiException(403, "owner_only"); g.deleted = now(); return buildJsonObject { put("ok", true) } }
            rest == "/members" && method == "GET" -> {
                if (mine?.state != "active") throw ApiException(403, "not_member")
                val visible = g.members.filter { (_, v) -> v.state in setOf("active", "keying") || (admin(g, me) && v.state in setOf("invited", "banned")) }
                return buildJsonObject { put("members", JsonArray(visible.map { (a, v) -> buildJsonObject {
                    put("address", a); handles[a]?.let { put("handle", it) }; put("role", v.role); put("state", v.state); v.invitedBy?.let { put("invited_by", it) }; put("revision", v.revision) } })) }
            }
            rest == "/members" && method == "POST" -> {
                if (!(admin(g, me) || (g.membersCanInvite && mine?.state == "active"))) throw ApiException(403, "not_admin")
                val results = (body["invite"] as JsonArray).map { it.jsonPrimitive.content }.map { raw ->
                    val address = handles.entries.firstOrNull { it.value == raw || it.value == "$raw/main" }?.key ?: raw
                    val status = when {
                        address !in devices -> "unresolved"
                        g.members[address]?.state in setOf("active", "keying", "invited") -> "already_member"
                        else -> { g.members[address] = Member("member", "invited", invitedBy = me); notice(g, "member_invited", me, buildJsonObject { put("address", address) }); "invited" }
                    }
                    buildJsonObject { put("address", address); put("status", status) }
                }
                return buildJsonObject { put("results", JsonArray(results)) }
            }
            rest.startsWith("/members/") && method == "PATCH" -> {
                val target = java.net.URLDecoder.decode(rest.removePrefix("/members/"), "UTF-8")
                val t = g.members[target] ?: throw ApiException(404, "not_found")
                if (!admin(g, me) && !(target == me && s("role") == "member")) throw ApiException(403, "not_admin")
                if (body.long("expected_revision") != t.revision) throw ApiException(409, "revision_conflict")
                s("role")?.let { t.role = it; notice(g, "role_changed", me, buildJsonObject { put("address", target); put("role", it) }) }
                s("state")?.let { st ->
                    if (t.state == "invited") g.members.remove(target)
                    else { t.state = st; notice(g, if (st == "banned") "member_banned" else "member_removed", me, buildJsonObject { put("address", target) }) }
                }
                t.revision++; g.revision++
                return buildJsonObject { put("address", target) }
            }
            rest == "/invitation" -> {
                if (mine?.state != "invited") throw ApiException(403, "invite_required")
                if ((body["accept"] as? JsonPrimitive)?.booleanOrNull == true) mine.state = "keying" else g.members.remove(me)
                return buildJsonObject { put("ok", true) }
            }
            rest == "/join" -> {
                if (g.joinPolicy != "public" || s("link_token") != g.linkToken) throw ApiException(403, "invalid_link")
                g.members[me] = Member("member", "keying")
                return buildJsonObject { put("ok", true) }
            }
            rest == "/leave" -> {
                if (mine == null) throw ApiException(403, "not_member")
                mine.state = "left"; g.revision++
                notice(g, "member_left", me, buildJsonObject { put("address", me) })
                return buildJsonObject { put("ok", true) }
            }
            rest == "/link" -> {
                if (!admin(g, me)) throw ApiException(403, "not_admin")
                g.linkToken = if ((body["enabled"] as? JsonPrimitive)?.booleanOrNull == true) (1..22).map { "abcdefghijkmnpqrstuvwxyz23456789".random() }.joinToString("") else null
                return buildJsonObject { put("link_token", g.linkToken?.let(::JsonPrimitive) ?: JsonNull); g.linkToken?.let { put("url", "https://inbox.pigeonpost.dev/g/${g.id}#$it") } }
            }
            rest == "/read" -> { mine?.readCursor = maxOf(mine?.readCursor ?: 0, body.long("seq")); return buildJsonObject { put("ok", true) } }
            rest == "/notifications" -> { mine?.notify = s("notify") ?: "all"; mine?.mutedUntil = (body["muted_until"] as? JsonPrimitive)?.longOrNull; return buildJsonObject { put("ok", true) } }
            rest == "/transfer" -> {
                if (mine?.role != "owner") throw ApiException(403, "owner_only")
                val to = g.members[s("to")] ?: throw ApiException(404, "not_found")
                to.role = "owner"; mine.role = "admin"; g.revision++
                notice(g, "ownership_transferred", me, buildJsonObject { put("address", s("to")) })
                return buildJsonObject { put("ok", true) }
            }
            rest.startsWith("/messages/") && method == "DELETE" -> {
                val seq = rest.removePrefix("/messages/").toLong()
                val i = g.items.indexOfFirst { it.long("seq") == seq }
                if (i < 0) throw ApiException(404, "not_found")
                if (g.items[i].str("sender") != me && !admin(g, me)) throw ApiException(403, "not_admin")
                g.items[i] = JsonObject(g.items[i] + mapOf("ciphertext" to JsonNull, "deleted_at" to JsonPrimitive(now())))
                return buildJsonObject { put("ok", true) }
            }
        }
        return null
    }

    private suspend fun handle(me: String, method: String, rawPath: String, body: JsonObject, device: String?): JsonElement {
        val path = rawPath.substringBefore('?')
        val query = rawPath.substringAfter('?', "").split('&').filter { it.contains('=') }.associate { it.substringBefore('=') to it.substringAfter('=') }
        calls += Triple(me, method, path)
        beforeHandle?.invoke(me, method, path)
        fun s(k: String) = (body[k] as? JsonPrimitive)?.contentOrNull
        if (path == "/v1/mls/devices" && method == "POST") {
            val devs = devices.getValue(me)
            val did = s("device_id")!!
            if (did in devs) throw ApiException(409, "device_exists")
            val sigKey = unb64(s("signature_key")!!)
            devs[did] = Device(sigKey)
            val att = signAttestationWithSeed(seeds.getValue(me), attestationPayload(me, did, sigKey))
            return buildJsonObject { put("device_id", did); put("attestation", b64(att)); put("mailbox_public_key", b64(publicKeys.getValue(me))); put("fingerprint_digits", "") }
        }
        if (path == "/v1/mls/key-packages/count") {
            dev(me, device)
            var unclaimed = 0; var lastResort = false
            kps.values.filter { it.mailbox == me && it.deviceId == device }.forEach { if (it.lastResort) lastResort = true else if (it.claimedBy == null) unclaimed++ }
            return buildJsonObject { put("unclaimed", unclaimed); put("last_resort", lastResort) }
        }
        if (path == "/v1/mls/key-packages" && method == "POST") {
            val d = dev(me, device)
            val all = (body["key_packages"] as? JsonArray).orEmpty().map { it.jsonPrimitive.content to false } + listOfNotNull(s("last_resort")?.let { it to true })
            for ((k, lr) in all) {
                val info = inspectKeyPackage(unb64(k))
                if (info.mailbox != me || info.deviceId != device || hex(info.signatureKey) != hex(d.sigKey)) throw ApiException(400, "invalid_key_package")
                if (lr) kps.entries.removeAll { it.value.mailbox == me && it.value.deviceId == device && it.value.lastResort }
                kps[hex(info.keyPackageRef)] = Kp(me, device!!, k, lr, b64(info.keyPackageRef))
            }
            return buildJsonObject { put("stored", all.size) }
        }
        if (path == "/v1/mls/welcomes") {
            dev(me, device)
            return buildJsonObject { putJsonArray("welcomes") { welcomes.filter { it.mailbox == me && it.deviceId == device && !it.acked }.forEach { w ->
                add(buildJsonObject { put("welcome_id", w.id); put("group_id", w.groupId); put("epoch", w.epoch); put("seq", w.seq); put("welcome", w.welcome) }) } } }
        }
        Regex("^/v1/mls/welcomes/([^/]+)/ack$").find(path)?.let { m ->
            val w = welcomes.firstOrNull { it.id == m.groupValues[1] && it.mailbox == me } ?: throw ApiException(404, "not_found")
            w.acked = true
            if ((body["joined"] as? JsonPrimitive)?.booleanOrNull != true) group(w.groupId).stale += key(me, w.deviceId)
            return buildJsonObject { put("ok", true) }
        }
        if (path == "/v1/mls/devices" && method == "GET") {
            val who = query["mailbox"]?.let { java.net.URLDecoder.decode(it, "UTF-8") } ?: me
            return buildJsonObject { put("mailbox", who); putJsonArray("devices") { devices[who].orEmpty().filterValues { !it.revoked }.forEach { (id, d) -> add(buildJsonObject {
                // The real postbox stores the library's fingerprint; the fake derives a stand-in from the key.
                val fp = java.security.MessageDigest.getInstance("SHA-256").digest(("pigeonpost/fake-fingerprint\u0000$who\u0000$id").toByteArray() + d.sigKey)
                put("device_id", id); put("fingerprint", hex(fp)); put("fingerprint_digits", dev.pigeonpost.groupcrypto.fingerprintDigits(fp)); put("signature_key", b64(d.sigKey))
            }) } } }
        }
        Regex("^/v1/mls/devices/([^/]+)$").find(path)?.let { m ->
            devices[me]?.get(m.groupValues[1])?.revoked = true
            return buildJsonObject { put("ok", true) }
        }
        manage(me, method, path, query, body)?.let { return it }
        if (path == "/v1/groups" && method == "POST") {
            val gid = "grp_" + (1..26).map { "abcdefghijklmnopqrstuvwxyz234567".random() }.joinToString("")
            val g = G(gid, s("name") ?: "Group", me)
            g.members[me] = Member("owner", "keying")
            g.postPolicy = s("post_policy") ?: "everyone"
            g.joinPolicy = s("join_policy") ?: "private"
            g.description = s("description")
            g.ephemeral = (body["ephemeral_secs"] as? JsonPrimitive)?.longOrNull ?: 0
            groups[gid] = g
            val invited = (body["invite"] as? JsonArray).orEmpty().map { it.jsonPrimitive.content }.map { raw ->
                val address = handles.entries.firstOrNull { it.value == raw || it.value == "$raw/main" }?.key ?: raw
                if (address in devices) { g.members[address] = Member("member", "invited", invitedBy = me); address to "invited" } else raw to "unresolved"
            }
            return buildJsonObject { putJsonObject("group") { put("id", gid); put("name", g.name); put("activation_state", "provisioning"); put("revision", 1); put("epoch", 0) }
                put("invitations", JsonArray(invited.map { (a, st) -> buildJsonObject { put("address", a); put("status", st) } })) }
        }
        val m = Regex("^/v1/groups/([^/]+)(/.*)?$").find(path) ?: throw ApiException(404, "not_found")
        val g = group(java.net.URLDecoder.decode(m.groupValues[1], "UTF-8"))
        val rest = m.groupValues[2]
        val mem = g.members[me]
        when {
            rest == "/crypto" -> {
                dev(me, device)
                return buildJsonObject { put("epoch", g.epoch); put("revision", g.revision); put("activation_state", g.activation); put("in_group", key(me, device!!) in g.leaves)
                    put("pending", JsonArray(pending(g))); putJsonArray("lagging_devices") {} }
            }
            rest == "/key-packages/claim" -> {
                dev(me, device)
                if (mem == null || !(mem.state == "active" || (g.activation == "provisioning" && me == g.creator))) throw ApiException(403, "not_member")
                val claims = mutableListOf<JsonObject>()
                for (target in (body["mailboxes"] as JsonArray).map { it.jsonPrimitive.content }) {
                    for (p in pending(g).filter { it.str("kind") == "add" && it.str("mailbox") == target }) {
                        val did = p.str("device_id")
                        val pick = kps.values.firstOrNull { it.mailbox == target && it.deviceId == did && !it.lastResort && it.claimedBy == null }
                            ?: kps.values.firstOrNull { it.mailbox == target && it.deviceId == did && it.lastResort } ?: continue
                        if (!pick.lastResort) pick.claimedBy = key(me, device!!)
                        g.claims[pick.ref] = Triple(key(me, device!!), target, did)
                        claims += buildJsonObject { put("mailbox", target); put("device_id", did); put("key_package", pick.bytes); put("key_package_ref", pick.ref) }
                    }
                }
                return buildJsonObject { put("claims", JsonArray(claims)); putJsonArray("exhausted") {} }
            }
            rest == "/commits" -> {
                val d = dev(me, device)
                val creatorActivation = g.activation == "provisioning" && me == g.creator
                if (mem == null || !(mem.state == "active" || creatorActivation)) throw ApiException(403, "not_member")
                if (body.long("epoch") != g.epoch || body.long("expected_revision") != g.revision)
                    throw ApiException(409, "revision_conflict", problem = buildJsonObject { put("current_epoch", g.epoch); put("revision", g.revision) })
                val commit = unb64(s("commit")!!)
                verifyEnvelopeSignature(d.sigKey, commit, unb64(s("envelope_signature")!!))
                val env = inspectEnvelope(commit)
                if (env.kind != "commit" || env.groupId != g.id || env.senderMailbox != me || env.senderDevice != device || env.clientMessageId != s("commit_id") || env.epoch.toLong() != g.epoch)
                    throw ApiException(400, "invalid_commit")
                val pend = pending(g)
                val adds = (body["adds"] as? JsonArray).orEmpty().map { it.jsonObject }
                val removes = (body["removes"] as? JsonArray).orEmpty().map { it.jsonObject }
                for (a in adds) {
                    if (pend.none { it.str("kind") == "add" && it.str("mailbox") == a.str("mailbox") && it.str("device_id") == a.str("device_id") }) throw ApiException(403, "not_pending")
                    val c = g.claims[a.str("key_package_ref")]
                    if (c == null || c.first != key(me, device!!) || c.second != a.str("mailbox") || c.third != a.str("device_id")) throw ApiException(403, "unclaimed")
                }
                for (r in removes) if (pend.none { it.str("kind") == "remove" && it.str("mailbox") == r.str("mailbox") && it.str("device_id") == r.str("device_id") } && r.str("mailbox") != me) throw ApiException(403, "not_pending")
                if (adds.isNotEmpty()) {
                    val welcome = s("welcome") ?: throw ApiException(400, "welcome_required")
                    if (welcomeRecipients(unb64(welcome)).map(::b64).sorted() != adds.map { it.str("key_package_ref") }.sorted()) throw ApiException(400, "welcome_mismatch")
                }
                val item = push(g, buildJsonObject {
                    put("type", "commit"); put("epoch", g.epoch); put("sender", me); put("sender_device", device); put("commit_id", s("commit_id")); put("commit", s("commit"))
                    put("adds", JsonArray(adds.map { a -> buildJsonObject { put("mailbox", a.str("mailbox")); put("device_id", a.str("device_id")) } })); put("removes", JsonArray(removes))
                })
                val seq = item.long("seq")
                if (creatorActivation) { g.leaves += key(me, device!!); g.activation = "active"; mem!!.state = "active"; mem.firstSeq = seq + 1 }
                for (r in removes) { g.leaves -= key(r.str("mailbox"), r.str("device_id")); g.stale -= key(r.str("mailbox"), r.str("device_id")) }
                for (a in adds) {
                    g.leaves += key(a.str("mailbox"), a.str("device_id"))
                    val am = g.members.getValue(a.str("mailbox"))
                    if (am.state == "keying") { am.state = "active"; am.firstSeq = seq + 1 }
                    welcomes += W(id("wel"), g.id, g.epoch + 1, seq, s("welcome")!!, a.str("mailbox"), a.str("device_id"))
                }
                g.epoch += 1; g.revision += 1
                return buildJsonObject { put("seq", seq); put("epoch", g.epoch); put("revision", g.revision) }
            }
            rest == "/epoch-ack" -> return buildJsonObject { put("ok", true) }
            rest == "/resync" -> { g.stale += key(me, device!!); return buildJsonObject { put("ok", true) } }
            rest == "/read" -> return buildJsonObject { put("ok", true) }
            rest.startsWith("/messages") && method == "GET" -> {
                dev(me, device)
                if (mem == null || mem.state != "active") throw ApiException(403, "not_member")
                val after = query["after_seq"]?.toLong() ?: 0
                val limit = minOf(100, query["limit"]?.toInt() ?: 100)
                val visible = g.items.filter { it.long("seq") > after && it.long("seq") >= (mem.firstSeq ?: 0) - 1 }
                return buildJsonObject { put("items", JsonArray(visible.take(limit))); put("last_seq", g.lastSeq); put("epoch", g.epoch); put("more", visible.size > limit) }
            }
            rest == "/messages" && method == "POST" -> {
                val d = dev(me, device)
                if (mem == null || mem.state != "active") throw ApiException(403, "not_member")
                if (g.postPolicy == "admins" && mem.role !in setOf("owner", "admin")) throw ApiException(403, "posting_restricted")
                g.items.firstOrNull { it.str("type") == "application" && it.str("sender") == me && it.str("client_message_id") == s("client_message_id") }?.let { dup ->
                    return buildJsonObject { put("seq", dup.long("seq")); put("id", dup.str("id")) }
                }
                if (body.long("epoch") != g.epoch) throw ApiException(409, "epoch_conflict", problem = buildJsonObject { put("current_epoch", g.epoch) })
                if (pending(g).any { it.str("kind") == "remove" }) throw ApiException(409, "key_activation_pending")
                val ct = unb64(s("ciphertext")!!)
                verifyEnvelopeSignature(d.sigKey, ct, unb64(s("envelope_signature")!!))
                val env = inspectEnvelope(ct)
                if (env.kind != "application" || env.groupId != g.id || env.senderMailbox != me || env.senderDevice != device || env.clientMessageId != s("client_message_id")) throw ApiException(400, "invalid_envelope")
                if (s("enc_scheme") != ENC_SCHEME) throw ApiException(400, "encryption_unsupported")
                val expires = if (g.ephemeral > 0) now() + g.ephemeral else null
                val item = push(g, buildJsonObject {
                    put("type", "application"); put("id", id("gm")); put("epoch", g.epoch); put("sender", me); put("sender_device", device)
                    put("client_message_id", s("client_message_id")); put("ciphertext", s("ciphertext")); put("expires_at", expires?.let { JsonPrimitive(it) } ?: JsonNull)
                })
                return buildJsonObject { put("seq", item.long("seq")); put("id", item.str("id")); put("expires_at", expires?.let { JsonPrimitive(it) } ?: JsonNull) }
            }
        }
        throw ApiException(404, "not_found:$method $path")
    }

    private fun JsonObject.str(k: String) = (this[k] as JsonPrimitive).content
    private fun JsonObject.long(k: String) = (this[k] as? JsonPrimitive)?.longOrNull ?: -1
}

/** Ed25519 public-key derivation (RFC 8032), for test fixtures only: slow, not constant time. */
internal object Ed25519Public {
    private val P = java.math.BigInteger.TWO.pow(255) - java.math.BigInteger.valueOf(19)
    private val D = java.math.BigInteger.valueOf(-121665).mod(P) * java.math.BigInteger.valueOf(121666).modInverse(P) % P
    private fun inv(x: java.math.BigInteger) = x.modPow(P - java.math.BigInteger.TWO, P)
    private data class Pt(val x: java.math.BigInteger, val y: java.math.BigInteger, val z: java.math.BigInteger, val t: java.math.BigInteger)
    private fun add(a: Pt, b: Pt): Pt {
        val aa = (a.y - a.x) * (b.y - b.x) % P
        val bb = (a.y + a.x) * (b.y + b.x) % P
        val cc = java.math.BigInteger.TWO * a.t * b.t % P * D % P
        val dd = java.math.BigInteger.TWO * a.z * b.z % P
        val e = bb - aa; val f = dd - cc; val g = dd + cc; val h = bb + aa
        return Pt((e * f).mod(P), (g * h).mod(P), (f * g).mod(P), (e * h).mod(P))
    }
    private val B: Pt by lazy {
        val y = java.math.BigInteger.valueOf(4) * inv(java.math.BigInteger.valueOf(5)) % P
        val xx = (y * y - java.math.BigInteger.ONE) * inv(D * y * y + java.math.BigInteger.ONE) % P
        var x = xx.modPow((P + java.math.BigInteger.valueOf(3)) / java.math.BigInteger.valueOf(8), P)
        if ((x * x - xx).mod(P) != java.math.BigInteger.ZERO) x = x * java.math.BigInteger.TWO.modPow((P - java.math.BigInteger.ONE) / java.math.BigInteger.valueOf(4), P) % P
        if (x.testBit(0)) x = P - x
        Pt(x, y, java.math.BigInteger.ONE, x * y % P)
    }
    fun fromSeed(seed: ByteArray): ByteArray {
        val h = java.security.MessageDigest.getInstance("SHA-512").digest(seed)
        val a = h.copyOfRange(0, 32).also { it[0] = (it[0].toInt() and 248).toByte(); it[31] = ((it[31].toInt() and 127) or 64).toByte() }
        var s = java.math.BigInteger(1, a.reversedArray())
        var q = Pt(java.math.BigInteger.ZERO, java.math.BigInteger.ONE, java.math.BigInteger.ONE, java.math.BigInteger.ZERO)
        var p = B
        while (s.signum() > 0) { if (s.testBit(0)) q = add(q, p); p = add(p, p); s = s.shiftRight(1) }
        val zi = inv(q.z); val x = q.x * zi % P; val y = q.y * zi % P
        val out = y.toByteArray().reversedArray().copyOf(32)
        if (x.testBit(0)) out[31] = (out[31].toInt() or 0x80).toByte()
        return out
    }
}
