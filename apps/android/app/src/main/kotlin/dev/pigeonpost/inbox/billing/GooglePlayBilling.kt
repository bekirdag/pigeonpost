package dev.pigeonpost.inbox.billing

import android.app.Activity
import android.content.Context
import com.android.billingclient.api.*
import dev.pigeonpost.core.*
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.asSharedFlow
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import java.io.IOException
import java.lang.ref.WeakReference
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

/** One BillingClient per foreground app model. The activity is held weakly across rotation. */
class GooglePlayBilling(context: Context) : PlayBilling {
    private val events = MutableSharedFlow<PlayUpdate>(extraBufferCapacity = 32)
    override val updates = events.asSharedFlow()
    private var activity = WeakReference<Activity>(null)
    private val connection = Mutex()
    private val offers = mutableMapOf<String, Pair<ProductDetails, ProductDetails.SubscriptionOfferDetails>>()
    private val client = BillingClient.newBuilder(context.applicationContext)
        .setListener { result, purchases ->
            when (result.responseCode) {
                BillingClient.BillingResponseCode.OK -> events.tryEmit(PlayUpdate.Purchases(purchases.orEmpty().map(::purchase)))
                BillingClient.BillingResponseCode.USER_CANCELED -> events.tryEmit(PlayUpdate.Cancelled)
                else -> events.tryEmit(PlayUpdate.Failed(message(result)))
            }
        }
        .enablePendingPurchases(PendingPurchasesParams.newBuilder().enableOneTimeProducts().build())
        .enableAutoServiceReconnection()
        .build()

    fun attach(value: Activity) { activity = WeakReference(value) }
    fun detach(value: Activity) { if (activity.get() === value) activity.clear() }

    private suspend fun connect() = connection.withLock {
        if (client.isReady) return@withLock
        suspendCancellableCoroutine { continuation ->
            client.startConnection(object : BillingClientStateListener {
                override fun onBillingSetupFinished(result: BillingResult) {
                    if (!continuation.isActive) return
                    if (result.responseCode == BillingClient.BillingResponseCode.OK) continuation.resume(Unit)
                    else continuation.resumeWithException(IOException(message(result)))
                }
                override fun onBillingServiceDisconnected() {}
            })
        }
    }

    override suspend fun prices(products: List<String>, basePlan: String): List<PlayPrice> {
        connect()
        val params = QueryProductDetailsParams.newBuilder().setProductList(products.map {
            QueryProductDetailsParams.Product.newBuilder().setProductId(it).setProductType(BillingClient.ProductType.SUBS).build()
        }).build()
        val details = suspendCancellableCoroutine { continuation ->
            client.queryProductDetailsAsync(params) { result, response ->
                if (continuation.isActive) {
                    if (result.responseCode == BillingClient.BillingResponseCode.OK) continuation.resume(response.productDetailsList)
                    else continuation.resumeWithException(IOException(message(result)))
                }
            }
        }
        offers.clear()
        return details.mapNotNull { product ->
            val offer = product.subscriptionOfferDetails?.firstOrNull {
                it.basePlanId == basePlan && it.offerId == null && it.pricingPhases.pricingPhaseList.size == 1 &&
                    it.pricingPhases.pricingPhaseList.single().let { phase -> phase.billingPeriod == "P1Y" &&
                        phase.recurrenceMode == ProductDetails.RecurrenceMode.INFINITE_RECURRING && phase.priceAmountMicros > 0 }
            } ?: return@mapNotNull null
            offers[product.productId] = product to offer
            val phase = offer.pricingPhases.pricingPhaseList.single()
            PlayPrice(product.productId, phase.formattedPrice, phase.priceCurrencyCode, phase.priceAmountMicros)
        }.sortedBy { products.indexOf(it.productId) }
    }

    override suspend fun purchases(): List<PlayPurchase> {
        connect()
        return suspendCancellableCoroutine { continuation ->
            client.queryPurchasesAsync(QueryPurchasesParams.newBuilder().setProductType(BillingClient.ProductType.SUBS).build()) { result, purchases ->
                if (continuation.isActive) {
                    if (result.responseCode == BillingClient.BillingResponseCode.OK) continuation.resume(purchases.map(::purchase))
                    else continuation.resumeWithException(IOException(message(result)))
                }
            }
        }
    }

    override fun launch(productId: String, accountId: String) {
        val screen = activity.get()?.takeUnless { it.isFinishing || it.isDestroyed }
            ?: throw IOException("Keep Pigeonpost open to start checkout.")
        val (product, offer) = offers[productId] ?: throw IOException("Refresh Google Play prices before purchasing.")
        if (!client.isReady) throw IOException("Google Play disconnected. Restore purchases and try again.")
        val flow = BillingFlowParams.newBuilder().setObfuscatedAccountId(accountId)
            .setProductDetailsParamsList(listOf(BillingFlowParams.ProductDetailsParams.newBuilder()
                .setProductDetails(product).setOfferToken(offer.offerToken).build())).build()
        val result = client.launchBillingFlow(screen, flow)
        if (result.responseCode != BillingClient.BillingResponseCode.OK) throw IOException(message(result))
    }

    override fun close() { activity.clear(); client.endConnection() }

    private fun purchase(value: Purchase) = PlayPurchase(value.purchaseToken, value.products,
        value.accountIdentifiers?.obfuscatedAccountId, value.purchaseState != Purchase.PurchaseState.PURCHASED)

    private fun message(result: BillingResult): String = when (result.responseCode) {
        BillingClient.BillingResponseCode.BILLING_UNAVAILABLE -> "Google Play purchases are unavailable on this device or Play account."
        BillingClient.BillingResponseCode.ITEM_UNAVAILABLE -> "This subscription is not yet available in your Google Play store."
        BillingClient.BillingResponseCode.ITEM_ALREADY_OWNED -> "You already own this subscription in Google Play. Use Restore purchases."
        BillingClient.BillingResponseCode.NETWORK_ERROR, BillingClient.BillingResponseCode.SERVICE_UNAVAILABLE,
        BillingClient.BillingResponseCode.SERVICE_DISCONNECTED -> "Google Play could not connect. Check your connection and restore purchases."
        else -> "Google Play could not complete this request. Try Restore purchases before buying again."
    }
}
