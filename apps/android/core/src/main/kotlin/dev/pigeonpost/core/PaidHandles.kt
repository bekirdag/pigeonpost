package dev.pigeonpost.core

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

@Serializable
data class PlayHandle(
    @SerialName("product_id") val productId: String,
    val namespace: String? = null,
    @SerialName("expires_at") val expiresAt: Long,
    val state: String,
    val active: Boolean,
    @SerialName("auto_renewing") val autoRenewing: Boolean,
    val acknowledged: Boolean,
    @SerialName("test_purchase") val testPurchase: Boolean = false,
)
@Serializable
data class PlayCatalog(
    val available: Boolean,
    @SerialName("product_ids") val productIds: List<String>,
    @SerialName("base_plan_id") val basePlanId: String,
    @SerialName("max_handles") val maxHandles: Int,
    @SerialName("account_id") val accountId: String,
    val handles: List<PlayHandle> = emptyList(),
)
@Serializable
data class PlayClaim(val purchase: PlayHandle, val mailbox: String? = null)

interface PaidHandleApi {
    suspend fun playCatalog(): PlayCatalog
    suspend fun redeemPlayPurchase(token: String, name: String? = null): PlayClaim
    suspend fun assignPlayHandle(productId: String, name: String): PlayClaim
    suspend fun checkHandle(name: String): HandleAvailability
}

data class PlayPrice(val productId: String, val formatted: String, val currency: String, val micros: Long)
data class PlayPurchase(val token: String, val productIds: List<String>, val accountId: String?, val pending: Boolean)
sealed interface PlayUpdate {
    data class Purchases(val purchases: List<PlayPurchase>) : PlayUpdate
    data object Cancelled : PlayUpdate
    data class Failed(val message: String) : PlayUpdate
}
interface PlayBilling {
    val updates: Flow<PlayUpdate>
    suspend fun prices(products: List<String>, basePlan: String): List<PlayPrice>
    suspend fun purchases(): List<PlayPurchase>
    fun launch(productId: String, accountId: String)
    fun close() {}
}

data class PaidHandleState(
    val catalog: PlayCatalog? = null,
    val prices: List<PlayPrice> = emptyList(),
    val ownedProducts: Set<String> = emptySet(),
    val busy: Boolean = false,
    val awaitingPayment: Boolean = false,
    val name: String = "",
    val availability: HandleAvailability? = null,
    val notice: String? = null,
    val error: String? = null,
) {
    val active get() = catalog?.handles.orEmpty().filter { it.active }
    val unassigned get() = active.firstOrNull { it.namespace == null }
    val nextPrice get() = prices.firstOrNull { price -> price.productId !in ownedProducts && active.none { it.productId == price.productId } }
    val nameReady get() = validHandleName(name) && availability?.available == true && availability.name == tidyHandle(name)
    val canRegister get() = catalog?.available == true && !busy && !awaitingPayment && nameReady &&
        (unassigned != null || active.size < (catalog.maxHandles.coerceAtMost(10)) && nextPrice != null)
}

/** Google Play owns payment state; the server owns handle entitlements. Neither a billing callback
 * nor an availability check grants a name. Every restore is verified again by the server. */
class PaidHandleStore(
    private val api: PaidHandleApi,
    private val billing: PlayBilling,
    private val scope: CoroutineScope,
    private val onRegistered: () -> Unit = {},
    private val onSessionExpired: () -> Unit = {},
) {
    private val mutable = MutableStateFlow(PaidHandleState())
    val state = mutable.asStateFlow()
    private val operations = Mutex()
    private val jobs = mutableSetOf<Job>()
    private var epoch = 0L
    private var checkoutName: Triple<String, String, String>? = null // account, product, name

    init {
        scope.launch {
            billing.updates.collect { update ->
                if (state.value.catalog == null) return@collect
                when (update) {
                    PlayUpdate.Cancelled -> {
                        checkoutName = null
                        mutable.update { it.copy(awaitingPayment = false, notice = "Purchase cancelled. You were not charged.") }
                    }
                    is PlayUpdate.Failed -> mutable.update { it.copy(awaitingPayment = false, error = update.message) }
                    is PlayUpdate.Purchases -> run { accept(update.purchases); reloadCatalog() }
                }
            }
        }
    }

    fun reset() {
        epoch++
        jobs.toList().forEach { it.cancel() }
        jobs.clear()
        checkoutName = null
        mutable.value = PaidHandleState()
    }

    fun editName(value: String) {
        if (state.value.busy || state.value.awaitingPayment) return
        mutable.update { it.copy(name = value.take(34), availability = null, error = null, notice = null) }
    }

    fun check() {
        if (state.value.busy || state.value.awaitingPayment) return
        val wanted = tidyHandle(state.value.name)
        if (!validHandleName(wanted)) return
        run {
            val result = api.checkHandle(wanted)
            currentCoroutineContext().ensureActive()
            mutable.update { it.copy(name = wanted, availability = result) }
        }
    }

    fun restore() {
        if (state.value.busy) return
        run {
            reloadCatalog()
            val catalog = state.value.catalog ?: return@run
            if (!catalog.available) return@run
            // Recover paid slots before fetching prices: an unavailable catalog must not block restoration.
            val owned = billing.purchases()
            currentCoroutineContext().ensureActive()
            mutable.update { it.copy(ownedProducts = owned.flatMap { p -> p.productIds }.toSet()) }
            accept(owned)
            reloadCatalog()
            val prices = billing.prices(catalog.productIds, catalog.basePlanId)
            currentCoroutineContext().ensureActive()
            mutable.update { it.copy(prices = prices, error = it.error ?: if (prices.isEmpty()) "Google Play prices are unavailable. Check your Play account and try again." else null) }
        }
    }

    fun register() {
        val snapshot = state.value
        if (!snapshot.canRegister) return
        val name = tidyHandle(snapshot.name)
        val account = snapshot.catalog?.accountId ?: return
        run {
            val available = api.checkHandle(name)
            currentCoroutineContext().ensureActive()
            mutable.update { it.copy(availability = available) }
            if (!available.available) return@run
            val credit = snapshot.unassigned
            if (credit != null) {
                val claim = api.assignPlayHandle(credit.productId, name)
                currentCoroutineContext().ensureActive()
                showClaim(claim)
                reloadCatalog()
            } else {
                val product = snapshot.nextPrice?.productId ?: return@run
                checkoutName = Triple(account, product, name)
                // Save intent before opening Play. If the process dies, Play's owned-purchase
                // query recovers the payment and the server exposes an unassigned paid slot.
                mutable.update { it.copy(awaitingPayment = true) }
                try { billing.launch(product, account) }
                catch (failure: Exception) {
                    mutable.update { it.copy(awaitingPayment = false) }
                    throw failure
                }
            }
        }
    }

    private suspend fun reloadCatalog() {
        val catalog = api.playCatalog()
        currentCoroutineContext().ensureActive()
        require(catalog.maxHandles in 1..10 && catalog.productIds.size in 1..10 && catalog.productIds.distinct().size == catalog.productIds.size) { "The handle catalog could not be loaded." }
        mutable.update { it.copy(catalog = catalog) }
    }

    private suspend fun accept(purchases: List<PlayPurchase>) {
        val catalog = state.value.catalog ?: return
        mutable.update { it.copy(awaitingPayment = false, ownedProducts = it.ownedProducts + purchases.flatMap { p -> p.productIds }) }
        for (purchase in purchases) {
            if (purchase.productIds.size != 1 || purchase.productIds.single() !in catalog.productIds) continue
            if (purchase.accountId != catalog.accountId) {
                mutable.update { it.copy(error = "A Google Play purchase belongs to another Pigeonpost account. Sign in to the Pigeonpost account used at checkout to restore it.") }
                continue
            }
            if (purchase.pending) {
                mutable.update { it.copy(notice = "Payment is pending in Google Play. Your handle unlocks when payment completes.") }
                continue
            }
            val name = checkoutName?.takeIf { it.first == catalog.accountId && it.second == purchase.productIds.single() }?.third
            try {
                val claim = api.redeemPlayPurchase(purchase.token, name)
                currentCoroutineContext().ensureActive()
                showClaim(claim)
                if (name != null) checkoutName = null
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (expired: SessionExpired) { throw expired }
            catch (failure: Exception) {
                mutable.update { it.copy(error = (failure.message ?: "Could not verify the purchase.") + " Your payment can be recovered with Restore purchases. Do not buy it again.") }
            }
        }
    }

    private fun showClaim(result: PlayClaim) {
        val purchase = result.purchase
        mutable.update { it.copy(notice = when {
            !purchase.active -> "This subscription is not active. Check its payment status in Google Play."
            purchase.namespace == null -> "Your paid handle is ready. Choose an available name to finish registration; no further payment is needed."
            result.mailbox == null -> "/${purchase.namespace} is registered. Restore purchases to finish opening its inbox."
            else -> "/${purchase.namespace} is registered."
        }, name = if (purchase.namespace != null) "" else it.name, availability = if (purchase.namespace != null) null else it.availability) }
        if (purchase.active && purchase.namespace != null) onRegistered()
    }

    private fun run(block: suspend () -> Unit) {
        val generation = epoch
        mutable.update { it.copy(busy = true, error = null) }
        val job = scope.launch {
            operations.withLock {
                if (generation != epoch) return@withLock
                mutable.update { it.copy(busy = true, error = null) }
                try { block() }
                catch (cancelled: CancellationException) { throw cancelled }
                catch (expired: SessionExpired) { onSessionExpired() }
                catch (failure: Exception) { if (generation == epoch) mutable.update { it.copy(error = failure.message ?: "Could not load purchases.") } }
                finally { if (generation == epoch) mutable.update { it.copy(busy = false) } }
            }
        }
        jobs += job
        job.invokeOnCompletion { jobs.remove(job) }
    }
}
