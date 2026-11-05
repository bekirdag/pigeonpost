package dev.pigeonpost.core.nests

import dev.pigeonpost.core.ApiException
import dev.pigeonpost.core.PlayBilling
import dev.pigeonpost.core.PlayPrice
import dev.pigeonpost.core.PlayPurchase
import dev.pigeonpost.core.PlayUpdate
import dev.pigeonpost.core.SessionExpired
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock

/** The Nest being looked at: its header, files, approvals and usage. */
data class NestDetail(
    val view: NestView,
    val files: NestFiles? = null,
    val approvals: List<NestApproval> = emptyList(),
    val usage: NestUsage? = null,
)

data class NestState(
    val catalog: NestCatalog? = null,
    val snapshot: NestSnapshot? = null,
    /** Google Play prices for the sellable packages, by Play product id. Prices come from Play only. */
    val prices: Map<String, PlayPrice> = emptyMap(),
    /** Play did not return a product (not created yet, or not in this country). */
    val playUnavailable: Boolean = false,
    val busy: Boolean = false,
    /** The package whose Play checkout is open. */
    val awaitingPayment: String? = null,
    val detail: NestDetail? = null,
    val organizations: List<OrgSummary> = emptyList(),
    /** `null` = personal; otherwise the organization whose Nests are shown. */
    val context: OrgSummary? = null,
    val orgNests: OrgNests? = null,
    val notice: String? = null,
    val error: String? = null,
) {
    val funding get() = snapshot?.allowance?.funding
    val sellable get() = catalog?.packages.orEmpty().filter { it.sellable }
    fun priceFor(pkg: NestPackage) = prices[pkg.googleProductId]
    /** A purchase on Play is possible only for a package Play priced, and never while another is open. */
    fun canBuy(pkg: NestPackage) = !busy && awaitingPayment == null && catalog?.rails?.google == true &&
        priceFor(pkg) != null && funding?.`package` != pkg.code && (funding == null || funding?.source == "google" || funding?.source == "grant" || funding?.source == "test")
}

/**
 * Nest packages bought with Google Play, and the account's Nests (Nests Phase B).
 *
 * Google Play owns payment; the postbox owns what a payment is worth. Order is always: a server
 * purchase intent (which refuses a second charge), then Play checkout bound to the account's
 * obfuscated id, then the server claim — the server verifies with Google and acknowledges, never
 * this device. Purchases Play still reports as owned are claimed again on every [load], which
 * recovers a payment the app never saw finish (process death, closed app).
 */
class NestStore(
    private val api: NestApi,
    private val billing: PlayBilling?,
    private val scope: CoroutineScope,
    private val onSessionExpired: () -> Unit = {},
) {
    private val mutable = MutableStateFlow(NestState())
    val state = mutable.asStateFlow()
    private val operations = Mutex()
    private val jobs = mutableSetOf<Job>()
    private var epoch = 0L
    /** Play purchase tokens of Nest products this device's Play account owns, by product. */
    private var owned: Map<String, PlayPurchase> = emptyMap()

    init {
        billing?.let { b ->
            scope.launch {
                b.updates.collect { update ->
                    val catalog = state.value.catalog ?: return@collect
                    val products = catalog.packages.map { it.googleProductId }.toSet()
                    when (update) {
                        PlayUpdate.Cancelled -> if (state.value.awaitingPayment != null)
                            mutable.update { it.copy(awaitingPayment = null, notice = "Purchase cancelled. You were not charged.") }
                        is PlayUpdate.Failed -> if (state.value.awaitingPayment != null)
                            mutable.update { it.copy(awaitingPayment = null, error = update.message) }
                        is PlayUpdate.Purchases -> {
                            val ours = update.purchases.filter { p -> p.productIds.any { it in products } }
                            if (ours.isNotEmpty()) run { claim(ours) }
                        }
                    }
                }
            }
        }
    }

    fun reset() {
        epoch++
        jobs.toList().forEach { it.cancel() }
        jobs.clear()
        owned = emptyMap()
        mutable.value = NestState()
    }

    fun clearMessages() = mutable.update { it.copy(notice = null, error = null) }

    /** Catalog, Nests, Play recovery and prices. */
    fun load() = run {
        val catalog = api.nestPackages()
        val snapshot = api.nests()
        currentCoroutineContext().ensureActive()
        mutable.update { it.copy(catalog = catalog, snapshot = snapshot) }
        val play = billing ?: return@run
        if (!catalog.rails.google) return@run
        val products = catalog.packages.map { it.googleProductId }.toSet()
        val mine = play.purchases().filter { p -> p.productIds.size == 1 && p.productIds.single() in products }
        currentCoroutineContext().ensureActive()
        owned = mine.associateBy { it.productIds.single() }
        if (mine.isNotEmpty()) claim(mine)
        val sellable = catalog.packages.filter { it.sellable }.map { it.googleProductId }
        val prices = try { play.subscriptionPrices(sellable, "monthly", "P1M") } catch (cancelled: CancellationException) { throw cancelled } catch (_: Exception) { emptyList() }
        currentCoroutineContext().ensureActive()
        mutable.update { it.copy(prices = prices.associateBy { p -> p.productId }, playUnavailable = prices.size < sellable.size) }
    }

    /** Buy `pkg` on Google Play (or change to it, on the same Play subscription). */
    fun buy(pkg: NestPackage) {
        val snapshot = state.value
        if (!snapshot.canBuy(pkg)) return
        val play = billing ?: return
        run {
            val intent = api.nestIntent(pkg.code, "google")
            currentCoroutineContext().ensureActive()
            if (intent.googleProductId != pkg.googleProductId) error("The postbox offered a different product. Try again.")
            mutable.update { it.copy(awaitingPayment = pkg.code) }
            val current = snapshot.funding?.takeIf { it.source == "google" }
            val currentPkg = current?.let { f -> snapshot.catalog?.packages?.firstOrNull { it.code == f.`package` } }
            val oldToken = currentPkg?.let { owned[it.googleProductId]?.token }
            val upgrade = currentPkg?.let { pkg.rank > it.rank } ?: true
            try {
                if (oldToken != null) play.launchReplacing(pkg.googleProductId, intent.googleAccountId, oldToken, upgrade)
                else play.launch(pkg.googleProductId, intent.googleAccountId)
            } catch (failure: Exception) {
                mutable.update { it.copy(awaitingPayment = null) }
                throw failure
            }
        }
    }

    fun retrySetup(setupId: String) = run {
        val snapshot = api.retryNestSetup(setupId)
        mutable.update { it.copy(snapshot = snapshot) }
    }

    /** Claim verified-by-Play purchases with the postbox. Pending and foreign purchases grant nothing. */
    private suspend fun claim(purchases: List<PlayPurchase>) {
        val account = state.value.catalog?.googleAccountId
        mutable.update { it.copy(awaitingPayment = null) }
        for (purchase in purchases) {
            owned = owned + (purchase.productIds.single() to purchase)
            if (purchase.accountId != null && purchase.accountId != account) {
                mutable.update { it.copy(error = "A Google Play Nest subscription belongs to another Pigeonpost account. Sign in to that account to restore it.") }
                continue
            }
            if (purchase.pending) {
                mutable.update { it.copy(notice = "Payment is pending in Google Play. Your Nest is set up when payment completes.") }
                continue
            }
            try {
                val snapshot = api.claimNestGoogle(purchase.token)
                currentCoroutineContext().ensureActive()
                mutable.update { it.copy(snapshot = snapshot, notice = when {
                    snapshot.setups.isNotEmpty() -> "Paid. Your Nest is being set up."
                    else -> "Your Nest package is active."
                }) }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (expired: SessionExpired) { throw expired }
            catch (failure: ApiException) {
                mutable.update { it.copy(error = NestRules.explain(failure.code, failure.message) + " Your payment can be recovered with Restore purchases; do not buy again.") }
            }
        }
    }

    // ---- one Nest --------------------------------------------------------------------------------

    fun open(id: String) {
        if (!NestRules.validNestId(id)) return
        run { refreshDetail(id) }
    }

    fun close() = mutable.update { it.copy(detail = null) }

    private suspend fun refreshDetail(id: String) {
        val view = api.nest(id)
        val files = runCatching { api.nestFiles(id) }.getOrNull()
        val approvals = runCatching { api.nestApprovals(id).approvals }.getOrDefault(emptyList())
        val usage = runCatching { api.nestUsage(id) }.getOrNull()
        currentCoroutineContext().ensureActive()
        mutable.update { it.copy(detail = NestDetail(view, files, approvals, usage)) }
    }

    fun upload(name: String, bytes: ByteArray) {
        val id = state.value.detail?.view?.nestId ?: return
        if (bytes.isEmpty() || bytes.size > NestRules.MAX_UPLOAD_BYTES) {
            mutable.update { it.copy(error = NestRules.explain("too_large", null)) }
            return
        }
        run {
            val file = api.uploadNestFile(id, name, bytes)
            mutable.update { it.copy(notice = when (file.status) {
                "parsed" -> "${file.name} is ready for your Nest."
                "partial" -> "${file.name} was partly read. ${file.warnings.firstOrNull().orEmpty()}".trim()
                else -> "${file.name} could not be read: ${file.reason ?: "unsupported file"}."
            }) }
            refreshDetail(id)
        }
    }

    fun deleteFile(fileId: String) {
        val id = state.value.detail?.view?.nestId ?: return
        run { api.deleteNestFile(id, fileId); refreshDetail(id) }
    }

    /** Approve or reject exactly the card shown: the hash the person saw goes to the server. */
    fun decide(card: NestApproval, approve: Boolean) {
        val id = state.value.detail?.view?.nestId ?: return
        if (card.status != "pending") return
        run {
            api.decideNestApproval(id, card.approvalId, approve, card.argsHash)
            mutable.update { it.copy(notice = if (approve) "Approved." else "Rejected.") }
            refreshDetail(id)
        }
    }

    fun pause() = action("pause")
    fun resume() = action("resume")
    private fun action(what: String) {
        val id = state.value.detail?.view?.nestId ?: return
        run { api.nestAction(id, what); refreshDetail(id) }
    }

    fun rename(label: String) {
        val id = state.value.detail?.view?.nestId ?: return
        val clean = label.trim()
        if (clean.isEmpty() || clean.length > 64) return
        run { api.renameNest(id, clean); refreshDetail(id); mutable.update { it.copy(snapshot = api.nests()) } }
    }

    fun delete(confirm: String) {
        val view = state.value.detail?.view ?: return
        if (confirm != view.label) { mutable.update { it.copy(error = NestRules.explain("confirm_required", null)) }; return }
        run {
            api.deleteNest(view.nestId, confirm)
            mutable.update { it.copy(detail = null, notice = "${view.label} was deleted.", snapshot = api.nests()) }
        }
    }

    /** The export zip for `save`, which writes it where the person chose. */
    fun export(save: suspend (ByteArray) -> Unit) {
        val id = state.value.detail?.view?.nestId ?: return
        run {
            val bytes = api.exportNest(id)
            save(bytes)
            mutable.update { it.copy(notice = "Export saved.") }
        }
    }

    // ---- company context ---------------------------------------------------------------------------

    fun loadOrganizations() = run {
        val orgs = api.nestOrganizations()
        mutable.update { it.copy(organizations = orgs) }
    }

    fun choose(context: OrgSummary?) {
        mutable.update { it.copy(context = context, orgNests = null) }
        if (context != null) run {
            val nests = api.orgNests(context.id)
            mutable.update { if (it.context?.id == context.id) it.copy(orgNests = nests) else it }
        }
    }

    private fun run(block: suspend () -> Unit) {
        val generation = epoch
        mutable.update { it.copy(busy = true, error = null) }
        val job = scope.launch {
            operations.withLock {
                if (generation != epoch) return@withLock
                try { block() }
                catch (cancelled: CancellationException) { throw cancelled }
                catch (expired: SessionExpired) { onSessionExpired() }
                catch (failure: ApiException) { if (generation == epoch) mutable.update { it.copy(error = NestRules.explain(failure.code, failure.message)) } }
                catch (failure: Exception) { if (generation == epoch) mutable.update { it.copy(error = failure.message ?: "Something went wrong. Try again.") } }
                finally { if (generation == epoch) mutable.update { it.copy(busy = false) } }
            }
        }
        jobs += job
        job.invokeOnCompletion { jobs.remove(job) }
    }
}
