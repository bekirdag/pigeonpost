package dev.pigeonpost.core

import org.junit.Assert.*
import org.junit.Test

class MessageDeliveryTest {
    private val all = Features.MESSAGE_ACTIONS
    private val sent = ThreadMessage("sent", "Words are presentation only", true, 100)

    @Test fun receivedMessagesAndDisabledFeaturesNeverClaimSenderReceipts() {
        val seen = sent.copy(receipt = MessageReceipt("seen", 110, 120), agentStatus = MessageAgentStatus("working"))
        assertNull(seen.copy(outgoing = false).deliveryState(all))
        assertNull(seen.deliveryState(emptySet()))
        assertNull(seen.copy(system = true).deliveryState(all))
    }
    @Test fun receiptStatesComeFromMetadataAndReadPreferenceMasksSeen() {
        assertEquals(MessageDeliveryState.SENT, sent.deliveryState(all))
        assertEquals(MessageDeliveryState.DELIVERED, sent.copy(receipt = MessageReceipt("delivered")).deliveryState(all))
        val seen = sent.copy(receipt = MessageReceipt("seen", 110, 120))
        assertEquals(MessageDeliveryState.SEEN, seen.deliveryState(all))
        assertEquals(MessageDeliveryState.DELIVERED, seen.deliveryState(all, readReceipts = false))
        assertEquals(MessageDeliveryState.SENT, sent.copy(receipt = MessageReceipt("unknown", seenAt = 120)).deliveryState(all))
    }
    @Test fun agentStateIsExplicitAndDoneReturnsToReceiptTicks() {
        val delivered = sent.copy(receipt = MessageReceipt("delivered"))
        assertEquals(MessageDeliveryState.WORKING, delivered.copy(agentStatus = MessageAgentStatus("working")).deliveryState(all))
        assertEquals(MessageDeliveryState.AGENT_FAILED, delivered.copy(agentStatus = MessageAgentStatus("failed")).deliveryState(all))
        assertEquals(MessageDeliveryState.DELIVERED, delivered.copy(agentStatus = MessageAgentStatus("done")).deliveryState(all))
        assertEquals(MessageDeliveryState.DELIVERED, delivered.copy(agentStatus = MessageAgentStatus("working")).deliveryState(setOf(Features.RECEIPTS)))
        assertNull(delivered.deliveryState(setOf(Features.AGENT_STATUS)))
    }
    @Test fun localUncertaintyNeverLooksLikeASuccessfulDelivery() {
        val seen = sent.copy(receipt = MessageReceipt("seen"))
        assertEquals(MessageDeliveryState.SENDING, seen.copy(status = Delivery.SENDING).deliveryState(all))
        assertEquals(MessageDeliveryState.UNCONFIRMED, seen.copy(status = Delivery.FAILED).deliveryState(all))
    }
}
