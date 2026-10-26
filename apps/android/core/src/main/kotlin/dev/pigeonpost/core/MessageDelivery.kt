package dev.pigeonpost.core

enum class MessageDeliveryState { SENDING, UNCONFIRMED, SENT, DELIVERED, SEEN, WORKING, AGENT_FAILED }

/** Sender-side presentation only; never infer a recipient state from text or missing metadata. */
fun ThreadMessage.deliveryState(features: Set<String>, readReceipts: Boolean? = null): MessageDeliveryState? {
    if (!outgoing || system || features.none { it == Features.RECEIPTS || it == Features.AGENT_STATUS }) return null
    if (status == Delivery.SENDING) return MessageDeliveryState.SENDING
    if (status == Delivery.FAILED) return MessageDeliveryState.UNCONFIRMED
    if (Features.AGENT_STATUS in features) when (agentStatus?.state) {
        "working" -> return MessageDeliveryState.WORKING
        "failed" -> return MessageDeliveryState.AGENT_FAILED
    }
    if (Features.RECEIPTS !in features) return null
    return when (receipt?.state) {
        "seen" -> if (readReceipts == false) MessageDeliveryState.DELIVERED else MessageDeliveryState.SEEN
        "delivered" -> MessageDeliveryState.DELIVERED
        else -> MessageDeliveryState.SENT
    }
}
