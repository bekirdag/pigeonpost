package dev.pigeonpost.core.groups

import dev.pigeonpost.core.ApiException
import dev.pigeonpost.groupcrypto.inspectEnvelope
import dev.pigeonpost.groupcrypto.inspectKeyPackage
import dev.pigeonpost.groupcrypto.mailboxForSeed
import dev.pigeonpost.groupcrypto.signAttestationWithSeed
import dev.pigeonpost.groupcrypto.verifyEnvelopeSignature
import dev.pigeonpost.groupcrypto.welcomeRecipients
import kotlinx.serialization.json.*
import java.security.KeyPairGenerator
import java.security.SecureRandom
import java.security.spec.NamedParameterSpec
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
    class Member(var role: String, var state: String, var firstSeq: Long? = null)
    class G(val id: String, val name: String, val creator: String) {
        var revision = 1L; var epoch = 0L; var activation = "provisioning"; var lastSeq = 0L
        var postPolicy = "everyone"; var ephemeral = 0L
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

    /** RFC 8032 public key for a seed: the JDK's generator draws exactly the 32-byte seed from its random source. */
    private fun edPublic(seed: ByteArray): ByteArray {
        val fixed = object : SecureRandom() { override fun nextBytes(bytes: ByteArray) { seed.copyInto(bytes, 0, 0, bytes.size) } }
        val generator = KeyPairGenerator.getInstance("Ed25519").apply { initialize(NamedParameterSpec.ED25519, fixed) }
        val encoded = generator.generateKeyPair().public.encoded
        return encoded.copyOfRange(encoded.size - 32, encoded.size)
    }

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

    private suspend fun <T> record(me: String, path: String, work: suspend () -> T): T = try { work() } catch (e: ApiException) { errors += me to (e.code ?: ""); throw e }

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
        Regex("^/v1/mls/devices/([^/]+)$").find(path)?.let { m ->
            devices[me]?.get(m.groupValues[1])?.revoked = true
            return buildJsonObject { put("ok", true) }
        }
        if (path == "/v1/groups" && method == "POST") {
            val gid = "grp_" + (1..26).map { "abcdefghijklmnopqrstuvwxyz234567".random() }.joinToString("")
            val g = G(gid, s("name") ?: "Group", me)
            g.members[me] = Member("owner", "keying")
            g.postPolicy = s("post_policy") ?: "everyone"
            groups[gid] = g
            return buildJsonObject { putJsonObject("group") { put("id", gid); put("name", g.name); put("activation_state", "provisioning"); put("revision", 1); put("epoch", 0) }; putJsonArray("invitations") {} }
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
