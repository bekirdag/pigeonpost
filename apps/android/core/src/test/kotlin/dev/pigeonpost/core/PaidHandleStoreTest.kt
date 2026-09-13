@file:OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
package dev.pigeonpost.core

import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.test.*
import org.junit.Assert.*
import org.junit.Test
import java.io.IOException

class PaidHandleStoreTest {
    private val ids = (1..10).map { "pigeonpost.handle.${it.toString().padStart(2, '0')}" }
    private fun handle(product: String, name: String? = null) = PlayHandle(product, name, 2_000_000_000, "SUBSCRIPTION_STATE_ACTIVE", true, true, true)
    private inner class Billing : PlayBilling {
        override val updates = MutableSharedFlow<PlayUpdate>(extraBufferCapacity = 10)
        var owned = emptyList<PlayPurchase>()
        var launched = mutableListOf<Pair<String, String>>()
        var pricesFail = false
        override suspend fun prices(products: List<String>, basePlan: String): List<PlayPrice> {
            if (pricesFail) throw IOException("Prices offline")
            return products.map { PlayPrice(it, "$8.00", "USD", 8_000_000) }
        }
        override suspend fun purchases() = owned
        override fun launch(productId: String, accountId: String) { launched += productId to accountId }
    }
    private open inner class Api : PaidHandleApi {
        var catalog = PlayCatalog(true, ids, "annual", 10, "account-hash")
        var redeemed = mutableListOf<String>()
        var assignments = 0
        var responseLost = false
        override suspend fun playCatalog() = catalog
        override suspend fun checkHandle(name: String) = HandleAvailability(name, true)
        override suspend fun redeemPlayPurchase(token: String, name: String?): PlayClaim {
            redeemed += token
            val item = catalog.handles.firstOrNull { it.productId == ids[0] } ?: handle(ids[0], name)
            catalog = catalog.copy(handles = catalog.handles.filterNot { it.productId == item.productId } + item)
            if (responseLost) { responseLost = false; throw IOException("Reply lost") }
            return PlayClaim(item, item.namespace?.let { "/$it/main" })
        }
        override suspend fun assignPlayHandle(productId: String, name: String): PlayClaim {
            assignments++
            val item = handle(productId, name)
            catalog = catalog.copy(handles = catalog.handles.filterNot { it.productId == productId } + item)
            return PlayClaim(item, "/$name/main")
        }
    }
    @Test fun checkoutRequiresAvailabilityAndDoubleTapLaunchesOnlyOnce() = runTest {
        val api = Api(); val billing = Billing(); val store = PaidHandleStore(api, billing, backgroundScope)
        store.restore(); runCurrent(); store.editName("Alex"); store.register(); runCurrent(); assertTrue(billing.launched.isEmpty())
        store.check(); runCurrent(); assertTrue(store.state.value.canRegister)
        store.register(); store.register(); runCurrent()
        assertEquals(listOf(ids[0] to "account-hash"), billing.launched)
        assertTrue(api.redeemed.isEmpty()); assertTrue(store.state.value.awaitingPayment)
        billing.updates.emit(PlayUpdate.Cancelled); runCurrent()
        assertFalse(store.state.value.awaitingPayment); assertTrue(store.state.value.active.isEmpty())
    }
    @Test fun pendingPaymentDoesNotGrantAndCompletionUsesTheCheckedName() = runTest {
        val api = Api(); val billing = Billing(); val store = PaidHandleStore(api, billing, backgroundScope)
        store.restore(); runCurrent(); store.editName("alex"); store.check(); runCurrent(); store.register(); runCurrent()
        val pending = PlayPurchase("token", listOf(ids[0]), "account-hash", true)
        billing.updates.emit(PlayUpdate.Purchases(listOf(pending))); runCurrent()
        assertTrue(api.redeemed.isEmpty()); assertTrue(store.state.value.active.isEmpty())
        billing.updates.emit(PlayUpdate.Purchases(listOf(pending.copy(pending = false)))); runCurrent()
        assertEquals(listOf("token"), api.redeemed); assertEquals("alex", store.state.value.active.single().namespace)
        assertEquals(1, billing.launched.size)
    }
    @Test fun reinstallRecoversAPaidSlotAndAssignsWithoutStartingCheckout() = runTest {
        val api = Api(); val billing = Billing(); billing.owned = listOf(PlayPurchase("paid-before-process-died", listOf(ids[0]), "account-hash", false))
        val store = PaidHandleStore(api, billing, backgroundScope)
        store.restore(); runCurrent(); assertNotNull(store.state.value.unassigned)
        billing.pricesFail = true; store.restore(); runCurrent()
        store.editName("new-name"); store.check(); runCurrent(); store.register(); runCurrent()
        assertEquals(1, api.assignments); assertTrue(billing.launched.isEmpty()); assertEquals("new-name", store.state.value.active.single().namespace)
    }
    @Test fun purchasesFromAnotherPigeonpostAccountAreNeverRedeemed() = runTest {
        val api = Api(); val billing = Billing(); billing.owned = listOf(PlayPurchase("other", listOf(ids[0]), "different-account", false))
        val store = PaidHandleStore(api, billing, backgroundScope); store.restore(); runCurrent()
        assertTrue(api.redeemed.isEmpty()); assertNotNull(store.state.value.error)
        assertEquals(ids[1], store.state.value.nextPrice?.productId)
    }
    @Test fun tenActiveSubscriptionsPreventAnEleventhCheckout() = runTest {
        val api = Api(); api.catalog = api.catalog.copy(handles = ids.mapIndexed { index, id -> handle(id, "name$index") })
        val billing = Billing(); val store = PaidHandleStore(api, billing, backgroundScope)
        store.restore(); runCurrent(); store.editName("eleventh"); store.check(); runCurrent(); store.register(); runCurrent()
        assertFalse(store.state.value.canRegister); assertTrue(billing.launched.isEmpty()); assertEquals(10, store.state.value.active.size)
    }
    @Test fun uncertainPaymentResponseRecoversWithoutAnotherCharge() = runTest {
        val api = Api(); api.responseLost = true
        val billing = Billing(); billing.owned = listOf(PlayPurchase("paid", listOf(ids[0]), "account-hash", false))
        val store = PaidHandleStore(api, billing, backgroundScope)
        store.restore(); runCurrent(); assertEquals(1, store.state.value.active.size)
        store.restore(); runCurrent(); assertEquals(1, store.state.value.active.size); assertTrue(billing.launched.isEmpty())
    }
    @Test fun signOutDiscardsLateCatalogResults() = runTest {
        val waiting = CompletableDeferred<PlayCatalog>()
        val api = object : Api() { override suspend fun playCatalog() = withContext(NonCancellable) { waiting.await() } }
        val store = PaidHandleStore(api, Billing(), backgroundScope)
        store.restore(); runCurrent(); store.reset(); waiting.complete(api.catalog); runCurrent()
        assertEquals(PaidHandleState(), store.state.value)
    }
    @Test fun aFreshOwnedPurchaseQueryReleasesExpiredProductSlots() = runTest {
        val api = Api(); val billing = Billing()
        billing.owned = listOf(PlayPurchase("pending", listOf(ids[0]), "account-hash", true))
        val store = PaidHandleStore(api, billing, backgroundScope); store.restore(); runCurrent()
        assertEquals(ids[1], store.state.value.nextPrice?.productId)
        billing.owned = emptyList(); store.restore(); runCurrent()
        assertEquals(ids[0], store.state.value.nextPrice?.productId)
    }
}
