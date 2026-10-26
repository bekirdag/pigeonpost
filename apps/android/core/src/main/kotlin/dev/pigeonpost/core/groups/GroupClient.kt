package dev.pigeonpost.core.groups

import dev.pigeonpost.core.ApiException
import dev.pigeonpost.core.WireJson
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.delay
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.serialization.json.JsonArray
import kotlinx.serialization.json.JsonElement
import kotlinx.serialization.json.JsonNull
import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.jsonObject
import kotlinx.serialization.json.longOrNull
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonArray
import java.security.SecureRandom
import java.security.MessageDigest
import java.util.Base64

const val ENC_SCHEME = "mls-rfc9420-x25519-aes128gcm-ed25519+ppgc1"
const val MAX_GROUP_FILE_BYTES = 25L * 1024 * 1024
private const val KEY_PACKAGE_TARGET = 20
private const val KEY_PACKAGE_FLOOR = 10
private const val HISTORY_LIMIT = 1000
private const val MAX_RETRIES = 6

/** A message as the person wrote it; it travels only inside the encryption. */
data class OutgoingDocument(val text: String, val files: List<OutgoingFile> = emptyList(), val replyTo: Long? = null, val editOf: Long? = null, val mentions: List<String> = emptyList())
class OutgoingFile(val name: String, val mime: String, val bytes: ByteArray)

sealed class GroupEvent {
    data class History(val groupId: String) : GroupEvent()
    data class Removed(val groupId: String) : GroupEvent()
    data class Resync(val groupId: String) : GroupEvent()
    data class Enrolled(val deviceId: String) : GroupEvent()
}

/**
 * The state machine every client runs against the postbox's group routes (docs/groups.md §3,
 * "Client loop"; the web reference is site-inbox/groups-core.js). It holds one device per mailbox.
 *
 * Everything that touches the MLS state runs one at a time under [lock]: the library holds one
 * pending commit per group and the state blob must be written after each mutation, in order.
 * `…Locked` functions expect the caller to hold it.
 */
class GroupClient(
    private val crypto: GroupCryptoLibrary,
    private val transport: GroupTransport,
    private val storage: GroupStorage,
    val mailbox: String,
    private val devicePrefix: String = "android",
    private val now: () -> Long = { System.currentTimeMillis() / 1000 },
    private val sleep: suspend (Long) -> Unit = { delay(it) },
    private val jitter: () -> Long = { (0..3000L).random() },
    private val onEvent: (GroupEvent) -> Unit = {},
) {
    private val lock = Mutex()
    private var device: GroupDeviceHandle? = null
    private lateinit var record: GroupRecord
    private val random = SecureRandom()
    private val reconciliationCursors = mutableMapOf<String, Long>()

    val deviceId: String? get() = if (::record.isInitialized) record.deviceId else null
    val persistent get() = storage.persistent

    // ---- bytes and ids ------------------------------------------------------------------------

    private fun b64(bytes: ByteArray): String = Base64.getEncoder().encodeToString(bytes)
    private fun unb64(text: String?): ByteArray = Base64.getDecoder().decode(text.orEmpty())
    private fun hex(bytes: ByteArray) = bytes.joinToString("") { "%02x".format(it) }
    private fun randomId(n: Int): String {
        val alphabet = "abcdefghijklmnopqrstuvwxyz234567"
        val bytes = ByteArray(n).also(random::nextBytes)
        return bytes.joinToString("") { alphabet[it.toInt() and 31].toString() }
    }
    private fun requestKey() = "rk_" + randomId(20)
    private fun summary(d: GroupDeviceInfo) = DeviceSummary(d.mailbox, d.deviceId, hex(d.fingerprint))
    private fun gpath(gid: String, rest: String = "") = "/v1/groups/" + java.net.URLEncoder.encode(gid, "UTF-8") + rest

    private suspend inline fun <reified T> call(method: String, path: String, body: JsonElement? = null, device: Boolean = false): T {
        val text = transport.groupJson(mailbox, method, path, body, if (device) record.deviceId else null)
        return WireJson.decodeFromString(text.ifBlank { "{}" })
    }
    private suspend fun send(method: String, path: String, body: JsonElement? = null, device: Boolean = false) {
        transport.groupJson(mailbox, method, path, body, if (device) record.deviceId else null)
    }

    // ---- the record -------------------------------------------------------------------------------

    private suspend fun persist() {
        val d = device ?: return
        record = record.copy(stateBlob = b64(d.exportState(unb64(record.stateKey))))
        // A save that starts must finish: a cancelled write would leave an older blob beside newer cursors.
        withContext(NonCancellable) { storage.save(mailbox, record) }
    }
    private fun cursorOf(gid: String) = record.groups[gid] ?: GroupCursor()
    private fun setCursor(gid: String, change: (GroupCursor) -> GroupCursor) {
        record = record.copy(groups = record.groups + (gid to change(cursorOf(gid))))
    }
    private fun historyOf(gid: String) = record.history[gid].orEmpty()
    private fun setHistory(gid: String, list: List<HistoryItem>) {
        record = record.copy(history = record.history + (gid to list))
    }
    private fun remember(gid: String, item: HistoryItem) {
        val list = historyOf(gid).toMutableList()
        val at = if (item.seq != null) list.indexOfFirst { it.seq == item.seq && it.kind == item.kind && it.after == item.after } else -1
        if (at >= 0) list[at] = item else list += item
        list.sortWith(compareBy<HistoryItem> { it.seq ?: Long.MAX_VALUE }.thenBy { it.after })
        setHistory(gid, if (list.size > HISTORY_LIMIT) list.takeLast(HISTORY_LIMIT) else list)
    }
    private fun updateHistory(gid: String, change: (HistoryItem) -> HistoryItem?) {
        setHistory(gid, historyOf(gid).mapNotNull(change))
    }

    // ---- device ---------------------------------------------------------------------------------

    /** Open this mailbox's device, or enrol a new one (docs/groups.md §1). Returns its device id. */
    suspend fun init(): String = lock.withLock {
        val saved = storage.load(mailbox)
        if (saved != null && saved.registered && saved.stateBlob != null) {
            try {
                device = crypto.importState(unb64(saved.stateBlob), unb64(saved.stateKey))
                record = saved
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) { device = null }
        }
        if (device == null) enrol()
        // Restored attempts require an explicit user retry. Never resume network work at launch.
        record = record.copy(history = record.history.mapValues { (_, history) -> history.map { item ->
            if (item.pending) item.copy(sendFailed = true, code = if (item.cmid in record.pendingSends) "send_unconfirmed" else "send_unavailable") else item
        } })
        record.deviceId
    }

    private suspend fun enrol() {
        val id = "$devicePrefix-" + randomId(12)
        val d = crypto.newDevice(mailbox, id)
        record = GroupRecord(deviceId = id, stateKey = b64(crypto.generateStateKey()))
        val res: RegisteredDevice = call("POST", "/v1/mls/devices", buildJsonObject {
            put("device_id", id); put("signature_key", b64(d.signaturePublicKey()))
        })
        d.installAttestation(unb64(res.mailboxPublicKey), unb64(res.attestation))
        device = d
        record = record.copy(registered = true, fingerprint = res.fingerprintDigits ?: crypto.fingerprintDigits(d.fingerprint()))
        persist()
        onEvent(GroupEvent.Enrolled(id))
    }

    /** A device the server no longer knows (revoked elsewhere) starts again. */
    suspend fun reEnrol() = lock.withLock {
        storage.remove(mailbox)
        device = null
        enrol()
    }

    /** Remove this device: the server revokes it, and its keys and history leave this phone. */
    suspend fun revoke() = lock.withLock {
        val id = record.deviceId
        try { send("DELETE", "/v1/mls/devices/" + java.net.URLEncoder.encode(id, "UTF-8"), device = true) }
        catch (failure: ApiException) { if (failure.status !in setOf(404, 403, 410) && failure.code != "unknown_device" && failure.code != "device_revoked") throw failure }
        storage.remove(mailbox)
        device = null
    }

    val fingerprint: String? get() = if (::record.isInitialized) record.fingerprint else null

    /** Keep at least ten single-use key packages and one last-resort package on the server. */
    suspend fun topUpKeyPackages(): Int = lock.withLock {
        val d = device ?: return@withLock 0
        val count: KeyPackageCount = call("GET", "/v1/mls/key-packages/count", device = true)
        if (count.unclaimed >= KEY_PACKAGE_FLOOR && count.lastResort) return@withLock 0
        val need = (KEY_PACKAGE_TARGET - count.unclaimed).coerceAtLeast(0)
        val packages = if (need > 0) d.generateKeyPackages(need) else emptyList()
        val lastResort = if (!count.lastResort) d.generateLastResortKeyPackage() else null
        persist()
        send("POST", "/v1/mls/key-packages", buildJsonObject {
            put("request_key", requestKey())
            putJsonArray("key_packages") { packages.forEach { add(JsonPrimitive(b64(it))) } }
            lastResort?.let { put("last_resort", b64(it)) }
        }, device = true)
        packages.size
    }

    // ---- welcomes -------------------------------------------------------------------------------

    suspend fun processWelcomes(): List<String> {
        val joined = lock.withLock {
            val d = device ?: return@withLock emptyList()
            val welcomes = call<WelcomesResponse>("GET", "/v1/mls/welcomes", device = true).welcomes.orEmpty()
            val out = mutableListOf<String>()
            for (w in welcomes) {
                var ok = false
                var code: String? = null
                try {
                    if (w.groupId in d.groups()) d.forgetGroup(w.groupId)
                    d.joinFromWelcome(unb64(w.welcome))
                    setCursor(w.groupId) { GroupCursor(cursor = w.seq.coerceAtLeast(0), inGroup = true, acked = 0) }
                    ok = true
                    out += w.groupId
                } catch (cancelled: CancellationException) { throw cancelled }
                catch (e: GroupCryptoError) { code = e.code }
                catch (_: Exception) { code = "join_failed" }
                persist()
                try {
                    send("POST", "/v1/mls/welcomes/" + java.net.URLEncoder.encode(w.id, "UTF-8") + "/ack", buildJsonObject {
                        put("request_key", requestKey()); put("joined", ok); code?.let { put("error_code", it) }
                    }, device = true)
                } catch (cancelled: CancellationException) { throw cancelled } catch (_: Exception) { /* the server retries the welcome */ }
                if (ok) {
                    remember(w.groupId, HistoryItem(kind = "notice", seq = w.seq, local = true, notice = "you_joined", createdAt = now()))
                    persist()
                    ackEpoch(w.groupId)
                }
            }
            out
        }
        // A last-resort package may have been the one used; a self-update replaces that leaf key at
        // once (P-2 §13.6). Only when no single-use packages are left, which is exactly when it was.
        if (joined.isNotEmpty()) {
            val count = try { lock.withLock { call<KeyPackageCount>("GET", "/v1/mls/key-packages/count", device = true) } }
                catch (cancelled: CancellationException) { throw cancelled } catch (_: Exception) { null }
            if (count != null && count.unclaimed == 0) for (gid in joined) try { selfUpdate(gid) } catch (cancelled: CancellationException) { throw cancelled } catch (_: Exception) {}
            joined.forEach { onEvent(GroupEvent.History(it)) }
        }
        return joined
    }

    private suspend fun ackEpoch(gid: String) {
        val d = device ?: return
        val epoch = try { d.epoch(gid) } catch (_: GroupCryptoError) { return }
        if (epoch <= cursorOf(gid).acked) return
        try {
            send("POST", gpath(gid, "/epoch-ack"), buildJsonObject { put("epoch", epoch) }, device = true)
            setCursor(gid) { it.copy(acked = epoch) }
        } catch (cancelled: CancellationException) { throw cancelled } catch (_: Exception) { /* retried at the next commit */ }
    }

    // ---- reading --------------------------------------------------------------------------------

    data class SyncResult(val processed: Int = 0, val removed: Boolean = false, val gap: Boolean = false, val resync: Boolean = false)

    suspend fun sync(gid: String): SyncResult = lock.withLock { syncLocked(gid, null) }

    /** Fetch and process everything after the cursor, strictly in sequence order; `upTo` stops early. */
    private suspend fun syncLocked(gid: String, upTo: Long?): SyncResult {
        if (!cursorOf(gid).inGroup) return SyncResult()
        var processed = 0
        for (page in 0 until 1000) {
            val res: ItemsResponse = try {
                call("GET", gpath(gid, "/messages?after_seq=${cursorOf(gid).cursor}&limit=100"), device = true)
            } catch (e: ApiException) {
                if (e.status == 410) { resyncLocked(gid); return SyncResult(processed, resync = true) }
                throw e
            }
            val items = res.items.orEmpty().sortedBy { it.seq }
            var stopped = false
            for (item in items) {
                if (upTo != null && item.seq > upTo) { stopped = true; break }
                if (item.seq <= cursorOf(gid).cursor) continue
                val outcome = processItem(gid, item)
                if (outcome == Outcome.GAP) { persist(); return SyncResult(processed, gap = true) }
                setCursor(gid) { it.copy(cursor = item.seq) }
                processed++
                persist()
                if (outcome == Outcome.REMOVED) { onEvent(GroupEvent.Removed(gid)); return SyncResult(processed, removed = true) }
                if (outcome == Outcome.COMMIT) { ackEpoch(gid); persist() }
            }
            if (stopped || !res.more || items.isEmpty()) break
        }
        if (processed > 0) onEvent(GroupEvent.History(gid))
        return SyncResult(processed)
    }

    private enum class Outcome { MESSAGE, COMMIT, NOTICE, REMOVED, GAP, SKIPPED }

    private fun isOwn(item: GroupItem) = item.sender == mailbox && item.senderDevice == record.deviceId

    private fun processItem(gid: String, item: GroupItem): Outcome {
        val d = device ?: return Outcome.SKIPPED
        when (item.type) {
            "notice" -> {
                remember(gid, HistoryItem(kind = "notice", seq = item.seq, notice = item.kind, actor = item.actor, metadata = item.metadata,
                    createdAt = item.createdAt, expiresAt = item.expiresAt))
                return Outcome.NOTICE
            }
            "commit" -> {
                if (isOwn(item) && !d.hasPendingCommit(gid)) return Outcome.SKIPPED // merged when the server accepted it
                val r = try { d.processIncoming(gid, unb64(item.commit)) }
                catch (e: GroupCryptoError) {
                    if (e.code == "future_epoch") return Outcome.GAP
                    // A commit this device cannot verify: it stops at this epoch and fails closed (P-2 §3).
                    remember(gid, HistoryItem(kind = "warning", seq = item.seq, after = 1, warning = "commit_rejected", code = e.code, createdAt = item.createdAt))
                    return Outcome.SKIPPED
                }
                if (r !is Processed.Commit) return Outcome.SKIPPED
                val o = r.result
                remember(gid, HistoryItem(kind = "commit", seq = item.seq, epoch = o.epoch, committer = summary(o.committer),
                    added = o.added.map(::summary), removed = o.removed.map(::summary), updated = o.updated.map(::summary), createdAt = item.createdAt))
                if (!sameDevices(o.added, item.adds) || !sameDevices(o.removed, item.removes))
                    remember(gid, HistoryItem(kind = "warning", seq = item.seq, after = 1, warning = "membership_mismatch", createdAt = item.createdAt))
                if (o.selfRemoved) {
                    try { d.forgetGroup(gid) } catch (_: GroupCryptoError) {}
                    setCursor(gid) { it.copy(inGroup = false) }
                    return Outcome.REMOVED
                }
                return Outcome.COMMIT
            }
            "application" -> {
                if (item.deletedAt != null || item.ciphertext == null) {
                    if (item.deletedAt != null) {
                        if (isOwn(item)) updateHistory(gid) { if (it.kind == "message" && it.cmid == item.clientMessageId) it.copy(seq = item.seq) else it }
                        eraseLocked(gid, setOf(item.seq), tombstones = true)
                    } else if (item.replacedBy != null && historyOf(gid).any { it.kind == "message" && it.seq == item.seq && it.text != null }) {
                        return Outcome.SKIPPED // Superseded ciphertext is different from deletion.
                    }
                    remember(gid, HistoryItem(kind = "message", seq = item.seq, sender = item.sender, deleted = true, createdAt = item.createdAt,
                        expiresAt = item.expiresAt))
                    return Outcome.MESSAGE
                }
                if (isOwn(item)) {
                    // Recorded when it was sent; the server copy only confirms its sequence number.
                    val pending = record.pendingSends[item.clientMessageId]
                    val request = pending?.request?.let { WireJson.parseToJsonElement(it).jsonObject }
                    if (pending != null && pending.group == gid && request?.get("ciphertext") == JsonPrimitive(item.ciphertext)) {
                        confirmPending(pending, Posted(item.seq, item.epoch, item.expiresAt), item.createdAt)
                    } else {
                        updateHistory(gid) { if (it.kind == "message" && it.cmid == item.clientMessageId && it.seq == item.seq)
                            it.copy(createdAt = item.createdAt, expiresAt = item.expiresAt) else it }
                    }
                    return Outcome.SKIPPED
                }
                val r = try { d.processIncoming(gid, unb64(item.ciphertext)) }
                catch (e: GroupCryptoError) {
                    if (e.code == "future_epoch") return Outcome.GAP
                    remember(gid, HistoryItem(kind = "message", seq = item.seq, sender = item.sender, undecryptable = true, code = e.code,
                        createdAt = item.createdAt, expiresAt = item.expiresAt))
                    return Outcome.MESSAGE
                }
                if (r !is Processed.Message) return Outcome.SKIPPED
                if (r.sender.mailbox != item.sender || r.sender.deviceId != item.senderDevice) {
                    remember(gid, HistoryItem(kind = "warning", seq = item.seq, after = 1, warning = "sender_mismatch", createdAt = item.createdAt))
                    return Outcome.SKIPPED
                }
                val doc = GroupBody.decode(r.body)
                val editMatches = doc.editOf == item.editOf?.toString()
                val replyMatches = doc.replyTo == item.replyTo?.toString()
                if (!editMatches || !replyMatches)
                    remember(gid, HistoryItem(kind = "warning", seq = item.seq, after = 1, warning = "action_mismatch", createdAt = item.createdAt))
                val files = doc.files.mapIndexed { i, f ->
                    val ref = r.attachments.getOrNull(i)
                    if (ref == null) f else f.copy(key = b64(ref.key), sha = b64(ref.sealedSha256), len = ref.plaintextLen)
                }
                remember(gid, HistoryItem(kind = "message", seq = item.seq, sender = r.sender.mailbox, senderDevice = r.sender.deviceId, cmid = item.clientMessageId,
                    text = doc.text, files = files, replyTo = doc.replyTo.takeIf { replyMatches }, editOf = doc.editOf.takeIf { editMatches },
                    actionsVerified = editMatches && replyMatches,
                    createdAt = item.createdAt, expiresAt = item.expiresAt))
                return Outcome.MESSAGE
            }
        }
        return Outcome.SKIPPED
    }

    private fun sameDevices(verified: List<GroupDeviceInfo>, declared: List<DeviceAddress>?): Boolean {
        if (declared == null) return true
        return verified.map { it.mailbox + "|" + it.deviceId }.sorted() == declared.map { it.mailbox + "|" + it.deviceId }.sorted()
    }

    private suspend fun resyncLocked(gid: String) {
        try { device?.forgetGroup(gid) } catch (_: GroupCryptoError) {}
        setCursor(gid) { it.copy(inGroup = false) }
        remember(gid, HistoryItem(kind = "warning", seq = cursorOf(gid).cursor, after = 1, warning = "resynced", createdAt = now()))
        persist()
        try { send("POST", gpath(gid, "/resync"), buildJsonObject { put("request_key", requestKey()) }, device = true) }
        catch (cancelled: CancellationException) { throw cancelled } catch (_: Exception) {}
        onEvent(GroupEvent.Resync(gid))
    }

    // ---- commits --------------------------------------------------------------------------------

    private class Plan(val bundle: CommitPlan, val adds: List<Claim>, val removes: List<DeviceAddress>, val revision: Long)

    /** Build, post and merge one commit; on a lost race discard, catch up, rebuild (backoff 200 ms … 10 s). */
    private suspend fun commitLocked(gid: String, activation: Boolean = false, plan: suspend () -> Plan?): Posted? {
        val d = device ?: return null
        for (attempt in 0 until MAX_RETRIES) {
            if (!activation) syncLocked(gid, null)
            if (!cursorOf(gid).inGroup) return null
            val prepared = plan() ?: return null
            persist() // the pending commit is part of the state
            try {
                val res: Posted = call("POST", gpath(gid, "/commits"), buildJsonObject {
                    put("request_key", requestKey()); put("epoch", prepared.bundle.epoch); put("commit_id", prepared.bundle.commitId)
                    put("commit", b64(prepared.bundle.commit)); put("envelope_signature", b64(prepared.bundle.envelopeSignature))
                    put("welcome", prepared.bundle.welcome?.let { JsonPrimitive(b64(it)) } ?: JsonNull)
                    putJsonArray("adds") { prepared.adds.forEach { c -> add(buildJsonObject { put("mailbox", c.mailbox); put("device_id", c.deviceId); put("key_package_ref", c.keyPackageRef) }) } }
                    putJsonArray("removes") { prepared.removes.forEach { r -> add(buildJsonObject { put("mailbox", r.mailbox); put("device_id", r.deviceId) }) } }
                    put("expected_revision", prepared.revision)
                }, device = true)
                // Anything sequenced before this commit is still in the old epoch: read it before merging.
                if (!activation) syncLocked(gid, res.seq - 1)
                val o = d.confirmCommit(gid)
                setCursor(gid) { it.copy(cursor = maxOf(it.cursor, res.seq)) }
                remember(gid, HistoryItem(kind = "commit", seq = res.seq, epoch = o.epoch, committer = summary(o.committer), added = o.added.map(::summary),
                    removed = o.removed.map(::summary), updated = o.updated.map(::summary), createdAt = now()))
                persist()
                ackEpoch(gid)
                persist()
                onEvent(GroupEvent.History(gid))
                return res
            } catch (e: Exception) {
                try { d.discardCommit(gid) } catch (_: GroupCryptoError) {}
                persist()
                if (e is CancellationException) throw e
                if (e is ApiException && e.status == 409 && (e.code == "revision_conflict" || e.code == "epoch_conflict")) {
                    sleep(minOf(10_000L, 200L shl attempt) + (0..100L).random())
                    continue
                }
                throw e
            }
        }
        throw ApiException(409, "revision_conflict")
    }

    private suspend fun cryptoStatus(gid: String): CryptoStatus = call("GET", gpath(gid, "/crypto"), device = true)

    /** Commit whatever the server says is pending: adds for keying members and new devices, removals for leavers. */
    suspend fun processPending(gid: String, noJitter: Boolean = false): Posted? {
        var status = lock.withLock { cryptoStatus(gid) }
        if (!status.inGroup && status.activationState == "active") return null
        if (status.pending.isNullOrEmpty()) return null
        if (!noJitter) {
            // Several online members see the same pending change; a little jitter lets one of them win cleanly.
            sleep(jitter())
            status = lock.withLock { cryptoStatus(gid) }
            if (status.pending.isNullOrEmpty()) return null
        }
        return lock.withLock { commitLocked(gid) { pendingPlan(gid, adds = true) } }
    }

    private suspend fun pendingPlan(gid: String, adds: Boolean): Plan? {
        val d = device ?: return null
        val fresh = cryptoStatus(gid)
        val pending = fresh.pending.orEmpty()
        val removes = pending.filter { it.kind == "remove" }.map { DeviceAddress(it.mailbox, it.deviceId) }
        val wanted = if (adds) pending.filter { it.kind == "add" } else emptyList()
        var claims = emptyList<Claim>()
        if (wanted.isNotEmpty()) {
            val claimed: ClaimsResponse = call("POST", gpath(gid, "/key-packages/claim"), buildJsonObject {
                put("request_key", requestKey()); putJsonArray("mailboxes") { wanted.map { it.mailbox }.distinct().forEach { add(JsonPrimitive(it)) } }
            }, device = true)
            // Only devices the server still lists as pending; never this device itself.
            val keys = wanted.map { it.mailbox + "|" + it.deviceId }.toSet()
            claims = claimed.claims.orEmpty().filter { (it.mailbox + "|" + it.deviceId) in keys && !(it.mailbox == mailbox && it.deviceId == record.deviceId) }
        }
        if (claims.isEmpty() && removes.isEmpty()) return null
        val bundle = d.commit(gid, claims.map { unb64(it.keyPackage) }, removes.map { DeviceKey(it.mailbox, it.deviceId) })
        return Plan(bundle, claims, removes, fresh.revision)
    }

    suspend fun selfUpdate(gid: String): Posted? = lock.withLock {
        commitLocked(gid) {
            val d = device ?: return@commitLocked null
            val fresh = cryptoStatus(gid)
            Plan(d.selfUpdate(gid), emptyList(), emptyList(), fresh.revision)
        }
    }

    // ---- creating -------------------------------------------------------------------------------

    /** Create the row, the MLS group at epoch 0, and post the first commit, which activates the group. */
    suspend fun createGroup(settings: JsonObject): CreatedGroup {
        val created: CreatedGroup = lock.withLock {
            call("POST", "/v1/groups", JsonObject(settings + ("request_key" to JsonPrimitive(requestKey()))))
        }
        activate(created.group.id)
        return created
    }

    suspend fun activate(gid: String): Posted? = lock.withLock {
        val d = device ?: return@withLock null
        if (gid !in d.groups()) {
            d.createGroup(gid)
            setCursor(gid) { GroupCursor(inGroup = true) }
            persist()
        }
        commitLocked(gid, activation = true) {
            val fresh = cryptoStatus(gid)
            if (fresh.activationState == "active" && d.epoch(gid) > 0) return@commitLocked null
            val adds = fresh.pending.orEmpty().filter { it.kind == "add" && !(it.mailbox == mailbox && it.deviceId == record.deviceId) }
            var claims = emptyList<Claim>()
            if (adds.isNotEmpty()) {
                val claimed: ClaimsResponse = call("POST", gpath(gid, "/key-packages/claim"), buildJsonObject {
                    put("request_key", requestKey()); putJsonArray("mailboxes") { adds.map { it.mailbox }.distinct().forEach { add(JsonPrimitive(it)) } }
                }, device = true)
                claims = claimed.claims.orEmpty().filter { !(it.mailbox == mailbox && it.deviceId == record.deviceId) }
            }
            val bundle = if (claims.isNotEmpty()) d.commit(gid, claims.map { unb64(it.keyPackage) }, emptyList()) else d.selfUpdate(gid)
            Plan(bundle, claims, emptyList(), fresh.revision)
        }
    }

    // ---- sending --------------------------------------------------------------------------------

    private fun fingerprint(gid: String, doc: OutgoingDocument): String {
        fun digest(bytes: ByteArray) = hex(MessageDigest.getInstance("SHA-256").digest(bytes))
        val payload = buildJsonObject {
            put("group", gid); put("text", doc.text)
            doc.replyTo?.let { put("reply_to", it) }; doc.editOf?.let { put("edit_of", it) }
            putJsonArray("mentions") { doc.mentions.distinct().sorted().forEach { add(JsonPrimitive(it)) } }
            putJsonArray("files") { doc.files.forEach { file -> add(buildJsonObject {
                put("name", file.name); put("mime", file.mime); put("sha", digest(file.bytes))
            }) } }
        }
        return digest(payload.toString().toByteArray(Charsets.UTF_8))
    }
    private fun setPending(pending: GroupPendingSend) {
        record = record.copy(pendingSends = record.pendingSends + (pending.cmid to pending))
    }
    private fun confirmPending(pending: GroupPendingSend, posted: Posted, at: Long = pending.createdAt) {
        val doc = GroupBody.decode(pending.body.toByteArray(Charsets.UTF_8))
        updateHistory(pending.group) { if (it.kind == "message" && it.cmid == pending.cmid) null else it }
        remember(pending.group, HistoryItem(kind = "message", seq = posted.seq, sender = mailbox,
            senderDevice = record.deviceId, cmid = pending.cmid, text = doc.text, files = pending.files,
            replyTo = doc.replyTo, editOf = doc.editOf, actionsVerified = true,
            createdAt = at, expiresAt = posted.expiresAt))
        record = record.copy(pendingSends = record.pendingSends - pending.cmid)
    }
    private suspend fun finishPending(pending: GroupPendingSend, posted: Posted): Posted {
        confirmPending(pending, posted)
        try { persist() } catch (failure: Exception) { setPending(pending); throw failure }
        onEvent(GroupEvent.History(pending.group))
        return posted
    }

    /** Prepare once, persisting the exact encrypted request before its first HTTP attempt. */
    suspend fun send(gid: String, doc: OutgoingDocument): Posted = lock.withLock {
        require(doc.replyTo == null || doc.editOf == null) { "Choose either reply or edit." }
        require(listOfNotNull(doc.replyTo, doc.editOf).all { it > 0 })
        require(doc.files.size <= 8)
        val intent = fingerprint(gid, doc)
        record.pendingSends.values.firstOrNull { it.group == gid && it.fingerprint == intent }?.let {
            return@withLock deliverPendingLocked(gid, it.cmid)
        }
        if (record.pendingSends.values.any { it.group == gid }) throw ApiException(409, "unconfirmed_send")
        if (record.pendingSends.size >= 64) throw ApiException(409, "outbox_full")
        val previous = doc.editOf?.let { seq ->
            val item = historyOf(gid).firstOrNull { it.kind == "message" && it.seq == seq }
                ?: throw ApiException(404, "reference_not_found")
            if (!GroupActions.editable(item, historyOf(gid), mailbox, now())) throw ApiException(403, "edit_window_closed")
            require(doc.files.isEmpty()) { "An edit keeps its original attachments." }
            item
        }
        val files = previous?.files?.toMutableList() ?: mutableListOf()
        for (file in doc.files) {
            require(file.bytes.size in 1..MAX_GROUP_FILE_BYTES) { "An attachment is larger than 25 MB." }
            val sealed = crypto.sealAttachment(file.bytes)
            val uploaded: Uploaded = WireJson.decodeFromString(transport.groupUpload(mailbox, gpath(gid, "/attachments"), sealed.sealed, record.deviceId))
            files += GroupFile(uploaded.id, file.name.take(200), file.mime.take(120), file.bytes.size.toLong(),
                b64(sealed.reference.key), b64(sealed.reference.sealedSha256), sealed.reference.plaintextLen)
        }
        val body = String(GroupBody.encode(doc.text, files, doc.replyTo?.toString(), doc.editOf?.toString()), Charsets.UTF_8)
        val pending = GroupPendingSend(gid, "cm_" + randomId(24), intent, now(), body, files, doc.mentions.distinct().sorted())
        setPending(pending)
        remember(gid, HistoryItem(kind = "message", pending = true, sender = mailbox, senderDevice = record.deviceId,
            cmid = pending.cmid, text = doc.text, files = files, replyTo = doc.replyTo?.toString(), editOf = doc.editOf?.toString(),
            actionsVerified = true, createdAt = pending.createdAt, expiresAt = previous?.expiresAt))
        persist()
        onEvent(GroupEvent.History(gid))
        deliverPendingLocked(gid, pending.cmid)
    }

    suspend fun retry(gid: String, cmid: String): Posted = lock.withLock { deliverPendingLocked(gid, cmid) }
    suspend fun discardPending(gid: String, cmid: String) = lock.withLock {
        if (record.pendingSends[cmid]?.group?.let { it != gid } == true) return@withLock
        record = record.copy(pendingSends = record.pendingSends - cmid)
        updateHistory(gid) { if (it.kind == "message" && it.cmid == cmid && it.pending) null else it }
        persist()
        onEvent(GroupEvent.History(gid))
    }
    private suspend fun deliverPendingLocked(gid: String, cmid: String): Posted {
        try {
            for (attempt in 0 until MAX_RETRIES) {
                var pending = record.pendingSends[cmid]?.takeIf { it.group == gid }
                    ?: throw ApiException(409, "send_unavailable")
                historyOf(gid).firstOrNull { it.kind == "message" && it.cmid == cmid && !it.pending && it.seq != null }?.let {
                    return finishPending(pending, Posted(it.seq!!, expiresAt = it.expiresAt))
                }
                if (now() < pending.createdAt || now() - pending.createdAt >= 86_400) throw ApiException(409, "send_retry_expired")
                if (pending.request == null) {
                    syncLocked(gid, null)
                    if (record.pendingSends[cmid] == null) throw ApiException(409, "send_unavailable")
                    if (!cursorOf(gid).inGroup) throw ApiException(403, "not_member")
                    val d = device ?: throw ApiException(403, "not_member")
                    val refs = pending.files.map { file ->
                        if (file.key == null || file.sha == null) throw GroupCryptoError("missing_key")
                        FileKey(unb64(file.key), unb64(file.sha), file.len ?: file.size)
                    }
                    val encrypted = d.encryptMessage(gid, cmid, pending.body.toByteArray(Charsets.UTF_8), refs)
                    val doc = GroupBody.decode(pending.body.toByteArray(Charsets.UTF_8))
                    val request = buildJsonObject {
                        put("request_key", "rk_$cmid"); put("client_message_id", cmid); put("epoch", encrypted.epoch); put("enc_scheme", crypto.encScheme)
                        put("ciphertext", b64(encrypted.ciphertext)); put("envelope_signature", b64(encrypted.envelopeSignature))
                        putJsonArray("attachments") { pending.files.forEach { add(JsonPrimitive(it.id)) } }
                        GroupActions.sequence(doc.replyTo)?.let { put("reply_to", it) }
                        GroupActions.sequence(doc.editOf)?.let { put("edit_of", it) }
                        if (pending.mentions.isNotEmpty()) putJsonArray("mentions") { pending.mentions.forEach { add(JsonPrimitive(it)) } }
                    }.toString()
                    val otherBytes = record.pendingSends.values.filter { it.cmid != cmid }.sumOf { (it.request?.toByteArray()?.size ?: 0) + it.body.toByteArray().size }
                    if (request.toByteArray().size + pending.body.toByteArray().size + otherBytes > 8 * 1024 * 1024) throw ApiException(409, "outbox_full")
                    pending = pending.copy(request = request)
                    setPending(pending)
                    // Advanced sender state and the exact request are saved together before transport.
                    persist()
                }
                try {
                    val result: Posted = call("POST", gpath(gid, "/messages"), WireJson.parseToJsonElement(pending.request!!), device = true)
                    return finishPending(pending, result)
                } catch (failure: ApiException) {
                    if (failure.status == 409 && failure.code in setOf("epoch_conflict", "key_activation_pending")) {
                        setPending(pending.copy(request = null))
                        persist()
                        if (failure.code == "key_activation_pending") commitLocked(gid) { pendingPlan(gid, adds = false) }
                        else sleep(50L * (attempt + 1))
                        continue
                    }
                    throw failure
                }
            }
            throw ApiException(409, "epoch_conflict")
        } catch (failure: Exception) {
            updateHistory(gid) { if (it.kind == "message" && it.cmid == cmid && it.pending)
                it.copy(sendFailed = true, code = (failure as? ApiException)?.code ?: "send_unconfirmed") else it }
            withContext(NonCancellable) { try { persist() } catch (_: Exception) { /* The already-saved request remains the recovery source. */ } }
            onEvent(GroupEvent.History(gid))
            throw failure
        }
    }

    /** Fetch a file's sealed bytes, check them against the hash in the message, and decrypt. */
    suspend fun openFile(gid: String, file: GroupFile): ByteArray {
        val key = file.key ?: throw GroupCryptoError("missing_key", "This file's key is not on this device.")
        val sealed = transport.groupDownload(mailbox, gpath(gid, "/attachments/" + java.net.URLEncoder.encode(file.id, "UTF-8")), record.deviceId, MAX_GROUP_FILE_BYTES + 4096)
        return crypto.openAttachment(sealed, FileKey(unb64(key), unb64(file.sha), file.len ?: file.size))
    }

    // ---- housekeeping ---------------------------------------------------------------------------

    private fun eraseLocked(gid: String, seqs: Set<Long>, tombstones: Boolean) {
        val history = historyOf(gid)
        val affected = GroupActions.linkedSequences(history, seqs)
        val cmids = history.filter { it.seq in affected || (it.pending && GroupActions.sequence(it.editOf) in affected) }.mapNotNull { it.cmid }.toSet()
        record = record.copy(pendingSends = record.pendingSends.filterValues { pending ->
            pending.group != gid || (pending.cmid !in cmids && GroupActions.sequence(GroupBody.decode(pending.body.toByteArray()).editOf) !in affected)
        })
        setHistory(gid, GroupActions.erasing(history, affected, tombstones))
    }

    /** Drop expired items by this device's own clock, whether or not the event arrived. */
    suspend fun pruneExpired(): Int = lock.withLock {
        if (!::record.isInitialized) return@withLock 0
        val t = now()
        var dropped = 0
        for (gid in record.history.keys.toList()) {
            val previous = historyOf(gid)
            val expired = previous.filter { (it.expiresAt ?: Long.MAX_VALUE) <= t }.mapNotNull { it.seq }.toSet()
            eraseLocked(gid, expired, tombstones = false)
            val next = historyOf(gid).filter { (it.expiresAt ?: Long.MAX_VALUE) > t }
            val expiredCmids = previous.filter { (it.expiresAt ?: Long.MAX_VALUE) <= t }.mapNotNull { it.cmid }.toSet()
            record = record.copy(pendingSends = record.pendingSends.filterValues { it.group != gid || it.cmid !in expiredCmids })
            setHistory(gid, next)
            dropped += previous.size - next.size
        }
        if (dropped > 0) persist()
        dropped
    }

    suspend fun dropSeqs(gid: String, seqs: Collection<Long>, tombstones: Boolean = false) = lock.withLock {
        eraseLocked(gid, seqs.toSet(), tombstones)
        persist()
    }

    /** One bounded metadata page per call; never decrypt or consume an old MLS ciphertext twice. */
    suspend fun reconcileDeletions(gid: String) = lock.withLock {
        val sequences = historyOf(gid).filter { it.kind == "message" }.mapNotNull { it.seq }
        val first = sequences.minOrNull() ?: return@withLock
        val through = sequences.maxOrNull() ?: return@withLock
        val after = reconciliationCursors[gid]?.takeIf { it in (first - 1)..through } ?: (first - 1)
        val page: ItemsResponse = call("GET", gpath(gid, "/messages?after_seq=$after&before_seq=${through + 1}&limit=100&metadata_only=true"), device = true)
        val deleted = page.items.orEmpty().filter { it.type == "application" && it.deletedAt != null }
        for (item in deleted) if (isOwn(item)) updateHistory(gid) { if (it.kind == "message" && it.cmid == item.clientMessageId) it.copy(seq = item.seq) else it }
        if (deleted.isNotEmpty()) { eraseLocked(gid, deleted.map { it.seq }.toSet(), tombstones = true); persist(); onEvent(GroupEvent.History(gid)) }
        val last = page.items.orEmpty().maxOfOrNull { it.seq }
        reconciliationCursors[gid] = if (page.more && last != null && last < through) last else (first - 1)
    }

    suspend fun receipts(gid: String): GroupReceipts = call("GET", gpath(gid, "/receipts"))

    suspend fun forget(gid: String) = lock.withLock {
        try { device?.forgetGroup(gid) } catch (_: GroupCryptoError) {}
        record = record.copy(groups = record.groups - gid, history = record.history - gid,
            pendingSends = record.pendingSends.filterValues { it.group != gid })
        reconciliationCursors.remove(gid)
        persist()
    }

    fun inGroup(gid: String) = ::record.isInitialized && record.groups[gid]?.inGroup == true
    fun history(gid: String): List<HistoryItem> = if (::record.isInitialized) record.history[gid].orEmpty() else emptyList()
    fun pendingIds(gid: String): Set<String> = if (::record.isInitialized) record.pendingSends.values.filter { it.group == gid }.map { it.cmid }.toSet() else emptySet()
    fun members(gid: String): List<DeviceSummary> = try { device?.members(gid).orEmpty().map(::summary) } catch (_: GroupCryptoError) { emptyList() }
    fun safetyNumber(gid: String): String? = try { device?.safetyNumber(gid) } catch (_: GroupCryptoError) { null }
    fun epoch(gid: String): Long? = try { device?.epoch(gid) } catch (_: GroupCryptoError) { null }
    fun digits(fingerprintHex: String): String = try {
        crypto.fingerprintDigits(fingerprintHex.chunked(2).map { it.toInt(16).toByte() }.toByteArray())
    } catch (_: Exception) { fingerprintHex }
}

/** The JSON document inside a group message (the web inbox's `encodeBody`/`decodeBody`). */
object GroupBody {
    data class Doc(val type: String, val text: String, val files: List<GroupFile>, val replyTo: String?, val editOf: String?)

    fun encode(text: String, files: List<GroupFile>, replyTo: String? = null, editOf: String? = null): ByteArray = buildJsonObject {
        put("v", 1); put("type", "text"); put("text", text.take(64000))
        if (files.isNotEmpty()) putJsonArray("files") { files.forEach { f -> add(buildJsonObject {
            put("id", f.id); put("name", f.name.take(200)); put("mime", f.mime.take(120)); put("size", f.size)
        }) } }
        replyTo?.let { put("reply_to", it) }
        editOf?.let { put("edit_of", it) }
    }.toString().toByteArray(Charsets.UTF_8)

    fun decode(bytes: ByteArray): Doc {
        val text = bytes.toString(Charsets.UTF_8)
        val root = try { WireJson.parseToJsonElement(text) as? JsonObject } catch (_: Exception) { null }
            ?: return Doc("text", text, emptyList(), null, null)
        fun str(o: JsonObject, k: String) = (o[k] as? JsonPrimitive)?.takeIf { it.isString }?.contentOrNull
        val files = (root["files"] as? JsonArray).orEmpty().mapNotNull { it as? JsonObject }.take(32).map { f ->
            GroupFile(str(f, "id").orEmpty(), str(f, "name") ?: "file", str(f, "mime") ?: "application/octet-stream", (f["size"] as? JsonPrimitive)?.longOrNull ?: 0)
        }
        return Doc(str(root, "type") ?: "text", str(root, "text").orEmpty(), files, str(root, "reply_to"), str(root, "edit_of"))
    }
}
