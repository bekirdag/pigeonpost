package dev.pigeonpost.core

import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.JsonObject
import java.security.SecureRandom

/** Server metadata is separate from body text and never grants request autonomy. */
@Serializable
data class MessageReply(
    @SerialName("msg_ref") val msgRef: String? = null,
    @SerialName("message_id") val messageId: String? = null,
    @SerialName("from") val sender: String? = null,
    val direction: String? = null,
    val excerpt: String? = null,
    val quote: String? = null,
    val verb: String? = null,
    val args: JsonObject? = null,
)

@Serializable
data class MessageReceipt(
    val state: String,
    @SerialName("delivered_at") val deliveredAt: Long? = null,
    @SerialName("seen_at") val seenAt: Long? = null,
)

@Serializable
data class MessageAgentStatus(val state: String, val at: Long? = null)

@Serializable
data class MessageVersion(@SerialName("message_id") val id: String, val body: String, val at: Long)

@Serializable
data class MessageInfo(
    @SerialName("message_id") val messageId: String,
    @SerialName("msg_ref") val msgRef: String? = null,
    val direction: String? = null,
    @SerialName("sent_at") val sentAt: Long? = null,
    val versions: List<MessageVersion> = emptyList(),
    @SerialName("reply_to") val replyTo: MessageReply? = null,
    val receipt: MessageReceipt? = null,
    @SerialName("agent_status") val agentStatus: MessageAgentStatus? = null,
)

/** Keep one instance for every attempt to deliver the same body and action. */
data class MessageSendAction(
    val msgRef: String = newMessageReference(),
    val replyTo: String? = null,
    val quote: String? = null,
    val editOf: String? = null,
) {
    init {
        require(MESSAGE_REFERENCE.matches(msgRef)) { "Invalid message reference." }
        require(replyTo == null || editOf == null) { "A message cannot be both an edit and a reply." }
        require(listOfNotNull(replyTo, editOf).all { MESSAGE_REFERENCE_OR_ID.matches(it) }) { "Invalid referenced message." }
        require(quote == null || replyTo != null) { "A quote needs a reply reference." }
        require(quote == null || quote.codePointCount(0, quote.length) <= 200) { "A reply quote is too long." }
    }
}

private val MESSAGE_REFERENCE = Regex("mref_[a-z2-7]{26}")
private val MESSAGE_REFERENCE_OR_ID = Regex("[A-Za-z0-9_:-]{1,256}")
private val messageRandom = SecureRandom()

/** 128 random bits, lowercase base32 with two zero padding bits, as required by the postbox. */
fun newMessageReference(): String {
    val bytes = ByteArray(16).also { messageRandom.nextBytes(it) }
    val alphabet = "abcdefghijklmnopqrstuvwxyz234567"
    var buffer = 0
    var bits = 0
    return buildString(31) {
        append("mref_")
        bytes.forEach { byte ->
            buffer = (buffer shl 8) or (byte.toInt() and 255)
            bits += 8
            while (bits >= 5) {
                bits -= 5
                append(alphabet[(buffer ushr bits) and 31])
            }
        }
        if (bits > 0) append(alphabet[(buffer shl (5 - bits)) and 31])
    }
}
