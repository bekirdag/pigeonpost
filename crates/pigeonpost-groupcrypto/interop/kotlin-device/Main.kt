// Kotlin/JVM interop agent: the generated UniFFI Kotlin binding (bindings/kotlin) over JNA, as
// the Android app uses it, run on a desktop JVM. Protocol: interop/run_interop.py.
import dev.pigeonpost.groupcrypto.*
import java.util.Base64

// --- a minimal JSON reader/writer, enough for the agent protocol ---
class JsonReader(private val s: String) {
    private var i = 0
    private fun ws() { while (i < s.length && s[i].isWhitespace()) i++ }
    fun value(): Any? {
        ws()
        return when (s[i]) {
            '{' -> obj()
            '[' -> arr()
            '"' -> str()
            't' -> { i += 4; true }
            'f' -> { i += 5; false }
            'n' -> { i += 4; null }
            else -> num()
        }
    }
    private fun obj(): Map<String, Any?> {
        val m = LinkedHashMap<String, Any?>(); i++; ws()
        if (s[i] == '}') { i++; return m }
        while (true) {
            ws(); val k = str(); ws(); i++ // ':'
            m[k] = value(); ws()
            if (s[i++] == '}') return m
        }
    }
    private fun arr(): List<Any?> {
        val l = ArrayList<Any?>(); i++; ws()
        if (s[i] == ']') { i++; return l }
        while (true) {
            l.add(value()); ws()
            if (s[i++] == ']') return l
        }
    }
    private fun str(): String {
        val b = StringBuilder(); i++
        while (s[i] != '"') {
            if (s[i] == '\\') {
                i++
                when (s[i]) {
                    'n' -> b.append('\n'); 't' -> b.append('\t'); 'r' -> b.append('\r')
                    'u' -> { b.append(s.substring(i + 1, i + 5).toInt(16).toChar()); i += 4 }
                    else -> b.append(s[i])
                }
            } else b.append(s[i])
            i++
        }
        i++
        return b.toString()
    }
    private fun num(): Long {
        val st = i
        while (i < s.length && (s[i] == '-' || s[i].isDigit())) i++
        return s.substring(st, i).toLong()
    }
}

fun json(v: Any?): String = when (v) {
    null -> "null"
    is String -> "\"" + v.replace("\\", "\\\\").replace("\"", "\\\"").replace("\n", "\\n") + "\""
    is Boolean, is Number -> v.toString()
    is ULong -> v.toString()
    is UInt -> v.toString()
    is Map<*, *> -> v.entries.joinToString(",", "{", "}") { json(it.key.toString()) + ":" + json(it.value) }
    is List<*> -> v.joinToString(",", "[", "]") { json(it) }
    else -> json(v.toString())
}

fun b64(b: ByteArray): String = Base64.getEncoder().encodeToString(b)
fun Map<String, Any?>.bytes(k: String): ByteArray = Base64.getDecoder().decode(this[k] as String)
fun Map<String, Any?>.str(k: String): String = this[k] as String

fun commitJson(b: CommitBundle) = mapOf(
    "epoch" to b.epoch, "commit_id" to b.commitId, "commit" to b64(b.commit),
    "welcome" to b.welcome?.let { b64(it) }, "signature" to b64(b.envelopeSignature),
)

fun attJson(a: AttachmentRef) = mapOf("key" to b64(a.key), "sha" to b64(a.sealedSha256), "len" to a.plaintextLen)

fun incomingJson(i: Incoming): Map<String, Any?> = when (i) {
    is Incoming.Message -> mapOf(
        "type" to "message", "body" to b64(i.message.body), "sender_mailbox" to i.message.sender.mailbox,
        "sender_device" to i.message.sender.deviceId, "client_message_id" to i.message.clientMessageId,
        "epoch" to i.message.epoch, "attachments" to i.message.attachments.map { attJson(it) },
    )
    is Incoming.Commit -> mapOf(
        "type" to "commit", "epoch" to i.outcome.epoch, "self_removed" to i.outcome.selfRemoved,
        "committer_mailbox" to i.outcome.committer.mailbox, "added" to i.outcome.added.size,
        "removed" to i.outcome.removed.size,
    )
    is Incoming.OwnCommitConfirmed -> mapOf("type" to "own_commit_confirmed", "epoch" to i.outcome.epoch)
    is Incoming.HistoryOffer -> mapOf("type" to "history_offer")
    is Incoming.Own -> mapOf("type" to "own")
}

var device: GroupDevice? = null

@Suppress("UNCHECKED_CAST")
fun handle(r: Map<String, Any?>): Map<String, Any?> {
    val d = device
    val g = r["group"] as String? ?: ""
    return when (r["op"]) {
        "init" -> {
            val n = GroupDevice.newWithIdentitySeed(r.bytes("seed"), r.str("device"))
            device = n
            mapOf("mailbox" to n.mailbox(), "signature_key" to b64(n.signaturePublicKey()))
        }
        "key_packages" -> {
            val kps = d!!.generateKeyPackages((r["count"] as Long).toUInt()).toMutableList()
            kps.add(d.generateLastResortKeyPackage())
            mapOf("key_packages" to kps.map { b64(it) })
        }
        "create" -> mapOf("epoch" to d!!.createGroup(g).epoch)
        "add" -> commitJson(d!!.addMembers(g, (r["key_packages"] as List<String>).map { Base64.getDecoder().decode(it) }))
        "remove_mailbox" -> commitJson(d!!.removeMailbox(g, r.str("mailbox")))
        "self_update" -> commitJson(d!!.selfUpdate(g))
        "confirm" -> mapOf("epoch" to d!!.confirmCommit(g).epoch)
        "discard" -> { d!!.discardCommit(g); mapOf() }
        "join" -> {
            val j = d!!.joinFromWelcome(r.bytes("welcome"))
            mapOf("group" to j.groupId, "epoch" to j.epoch, "members" to j.members.size)
        }
        "encrypt" -> {
            val atts = ArrayList<AttachmentRef>()
            var sealed: String? = null
            if (r["attachment"] is String) {
                val s = sealAttachment(r.bytes("attachment"))
                sealed = b64(s.sealed)
                atts.add(s.reference)
            }
            val m = d!!.encryptMessage(g, r.str("id"), r.bytes("body"), atts)
            mapOf("ciphertext" to b64(m.ciphertext), "signature" to b64(m.envelopeSignature), "epoch" to m.epoch, "sealed" to sealed)
        }
        "process" -> incomingJson(d!!.processIncoming(g, r.bytes("message")))
        "open_attachment" -> {
            val ref = r["ref"] as Map<String, Any?>
            val a = AttachmentRef(ref.bytes("key"), ref.bytes("sha"), (ref["len"] as Long).toULong())
            mapOf("plaintext" to b64(openAttachment(r.bytes("sealed"), a)))
        }
        "status" -> mapOf(
            "epoch" to d!!.epoch(g), "epoch_authenticator" to b64(d.epochAuthenticator(g)),
            "safety_number" to d.safetyNumber(g), "members" to d.members(g).size,
        )
        "persist" -> {
            val key = generateStateKey()
            val blob = d!!.exportState(key)
            device = GroupDevice.importState(blob, key)
            d.close()
            mapOf("bytes" to blob.size)
        }
        "snapshot" -> {
            val key = generateStateKey()
            mapOf("state" to b64(d!!.exportState(key)), "key" to b64(key))
        }
        "probe_snapshot" -> GroupDevice.importState(r.bytes("state"), r.bytes("key")).use { p ->
            try {
                mapOf("code" to "ok", "result" to incomingJson(p.processIncoming(g, r.bytes("message"))))
            } catch (e: GroupCryptoException.Failure) {
                mapOf("code" to e.code)
            }
        }
        else -> throw GroupCryptoException.Failure("invalid_input", "unknown op")
    }
}

fun main() {
    while (true) {
        val line = readLine() ?: break
        @Suppress("UNCHECKED_CAST")
        val req = JsonReader(line).value() as Map<String, Any?>
        if (req["op"] == "quit") break
        val resp = try {
            handle(req) + ("ok" to true)
        } catch (e: GroupCryptoException.Failure) {
            mapOf("ok" to false, "code" to e.code, "message" to e.detail)
        }
        println(json(resp))
        System.out.flush()
    }
}
