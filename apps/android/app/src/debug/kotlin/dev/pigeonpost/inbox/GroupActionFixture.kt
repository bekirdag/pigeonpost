package dev.pigeonpost.inbox

import dev.pigeonpost.core.*
import dev.pigeonpost.core.groups.*
import kotlinx.serialization.json.*
import java.io.IOException

/** Explicit debug transport over real MLS fixture accounts, with optional committed-response loss. */
internal class GroupActionFixture(
    private val mode: String,
    private val base: PostboxApi,
    handles: AccountHandleApi,
    private val world: FixtureGroups,
) : PostboxApi by base, AccountHandleApi by handles {
    private var reads = true
    private var lost = false
    private var holdReads = false
    // Capabilities are account-wide, so the shared inbox also negotiates folded reads. The
    // established direct-message fake has no edit rows; group content still uses real MLS below.
    override suspend fun inboxWithActions(identity: String, wait: Int?) = base.inbox(identity, wait)
    override suspend fun filteredInboxWithActions(identity: String) = base.filteredInbox(identity)
    override suspend fun ackWithEdits(identity: String, messageId: String) = base.ack(identity, messageId)
    override suspend fun capabilities() = Capabilities(listOf(Features.GROUPS, Features.DISAPPEARING) +
        if (mode == "group-actions-off") emptyList() else listOf(Features.MESSAGE_EDIT, Features.MESSAGE_REPLY, Features.RECEIPTS))
    override suspend fun policy(identity: String) = FilterPolicy(readReceipts = reads)
    override suspend fun setPolicy(identity: String, field: String, value: String): FilterPolicy {
        require(identity == world.me && field == "read_receipts")
        reads = value.toBooleanStrict()
        return policy(identity)
    }
    val transport: GroupTransport = object : GroupTransport by world.fake.transport {
        override suspend fun groupJson(mailbox: String, method: String, path: String, body: JsonElement?, device: String?): String {
            if (method == "GET" && path.endsWith("/receipts")) {
                val gid = path.removeSuffix("/receipts").substringAfterLast('/')
                val group = world.fake.groups[gid] ?: throw ApiException(404, "not_found")
                return buildJsonObject {
                    put("read_receipts", reads)
                    putJsonArray("members") { group.members.filter { it.key != mailbox && it.value.state == "active" }.forEach { (member, _) -> add(buildJsonObject {
                        put("address", member); world.fake.handles[member]?.let { put("handle", it) }
                        put("delivered_seq", group.lastSeq); put("seen_seq", if (reads) JsonPrimitive(group.lastSeq) else JsonNull)
                    }) } }
                }.toString()
            }
            // Keep the uncertain state stable for the retry interaction test. The retry POST still
            // reaches the real MLS fake and must recover its already-accepted request identity.
            if (holdReads && method == "GET" && path.contains("/messages")) throw IOException("Fixture read interruption")
            val response = world.fake.transport.groupJson(mailbox, method, path, body, device)
            if (method == "POST" && path.endsWith("/messages") && mailbox == world.me) {
                if (mode == "group-actions-lost" && !lost) {
                    lost = true; holdReads = true; throw IOException("Fixture committed response loss")
                }
                holdReads = false
            }
            return response
        }
    }
}
