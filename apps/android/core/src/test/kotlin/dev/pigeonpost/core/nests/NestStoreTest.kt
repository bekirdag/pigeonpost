@file:OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
package dev.pigeonpost.core.nests

import dev.pigeonpost.core.*
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.*
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import okhttp3.mockwebserver.MockResponse
import okhttp3.mockwebserver.MockWebServer
import org.junit.Assert.*
import org.junit.Test

class NestStoreTest {
    private val starter = NestPackage("nest.ai.starter.monthly", "Pigeonpost AI", rank = 2, slots = 1, sellable = true, googleProductId = "pigeonpost.nests.ai.starter")
    private val plus = NestPackage("nest.ai.plus.monthly", "Pigeonpost AI Plus", rank = 3, slots = 3, sellable = true, googleProductId = "pigeonpost.nests.ai.plus")
    private val pro = NestPackage("nest.ai.pro.monthly", "Pigeonpost AI Pro", rank = 4, slots = 10, sellable = false, googleProductId = "pigeonpost.nests.ai.pro")

    private val events = mutableListOf<String>()

    private inner class Billing : PlayBilling {
        override val updates = MutableSharedFlow<PlayUpdate>(extraBufferCapacity = 10)
        var owned = emptyList<PlayPurchase>()
        var available = true
        override suspend fun prices(products: List<String>, basePlan: String) = error("handle prices are not asked for")
        override suspend fun subscriptionPrices(products: List<String>, basePlan: String, period: String): List<PlayPrice> {
            assertEquals("monthly", basePlan); assertEquals("P1M", period)
            return if (available) products.map { PlayPrice(it, "$10.00", "USD", 10_000_000) } else emptyList()
        }
        override suspend fun purchases() = owned
        override fun launch(productId: String, accountId: String) { events += "launch:$productId:$accountId" }
        override fun launchReplacing(productId: String, accountId: String, oldPurchaseToken: String, upgrade: Boolean) {
            events += "replace:$productId:$oldPurchaseToken:$upgrade"
        }
    }

    private open inner class Api : NestApi {
        var snapshot = NestSnapshot()
        var intentRefusal: ApiException? = null
        val claimed = mutableListOf<String>()
        var decided: Triple<String, Boolean, String>? = null
        override suspend fun nestPackages() = NestCatalog("nests-2026-11", listOf(starter, plus, pro), NestRails(google = true), "acct-hash")
        override suspend fun nests() = snapshot
        override suspend fun nest(id: String) = NestView(id, "/k/nest", "Research", "running", "package", "ready")
        override suspend fun nestIntent(packageCode: String, rail: String): NestIntent {
            events += "intent:$packageCode:$rail"
            intentRefusal?.let { throw it }
            val pkg = listOf(starter, plus).first { it.code == packageCode }
            return NestIntent("npi_1", pkg.googleProductId, "acct-hash")
        }
        override suspend fun claimNestGoogle(purchaseToken: String): NestSnapshot {
            events += "claim:$purchaseToken"; claimed += purchaseToken
            snapshot = NestSnapshot(NestAllowance(NestFunding("ent_1", "google", starter.code), 1, 0), starter, setups = listOf(NestSetup("nsu_1", "Pigeonpost AI")))
            return snapshot
        }
        override suspend fun retryNestSetup(setupId: String) = snapshot
        override suspend fun nestFiles(id: String) = NestFiles()
        override suspend fun uploadNestFile(id: String, name: String, bytes: ByteArray): NestFile { events += "upload:$name:${bytes.size}"; return NestFile("nfl_1", name, bytes.size.toLong(), status = "parsed") }
        override suspend fun deleteNestFile(id: String, fileId: String) {}
        override suspend fun nestApprovals(id: String) = NestApprovals(listOf(card("nap_1", "pending"), card("nap_2", "superseded")))
        override suspend fun decideNestApproval(id: String, approvalId: String, approve: Boolean, argsHash: String) { decided = Triple(approvalId, approve, argsHash) }
        override suspend fun nestUsage(id: String) = NestUsage()
        override suspend fun nestAction(id: String, action: String) { events += "action:$action" }
        override suspend fun renameNest(id: String, label: String) {}
        override suspend fun deleteNest(id: String, confirm: String) { events += "delete:$confirm" }
        override suspend fun exportNest(id: String) = byteArrayOf(80, 75)
        override suspend fun nestOrganizations() = listOf(OrgSummary("org_1", "Wodo"))
        override suspend fun orgNests(orgId: String) = OrgNests(listOf(OrgNest("nst_00ff", "/wodo/hr", "HR", "running", "operator")))
    }

    private fun card(id: String, status: String) = NestApproval(id, "send_message",
        buildJsonObject { put("tool", "send_message") }, "hash-$id", status)

    @Test fun purchaseAsksTheServerFirstThenPlayThenClaimsAndNeverAcknowledgesOnDevice() = runTest {
        val api = Api(); val billing = Billing(); val store = NestStore(api, billing, backgroundScope)
        store.load(); runCurrent()
        assertTrue(store.state.value.canBuy(starter))
        store.buy(starter); store.buy(starter); runCurrent()
        assertEquals(listOf("intent:${starter.code}:google", "launch:${starter.googleProductId}:acct-hash"), events)
        assertEquals(starter.code, store.state.value.awaitingPayment)
        billing.updates.emit(PlayUpdate.Purchases(listOf(PlayPurchase("tok-1", listOf(starter.googleProductId), "acct-hash", false)))); runCurrent()
        assertEquals("claim:tok-1", events.last())
        assertNull(store.state.value.awaitingPayment)
        assertEquals(1, store.state.value.snapshot?.setups?.size)
        // The Play adapter has no acknowledge call at all: the server acknowledges after recording.
        assertFalse(PlayBilling::class.java.methods.any { it.name.contains("acknowledge", ignoreCase = true) })
    }

    @Test fun ownedNestPurchasesAreClaimedOnLoadAndHandlesAreLeftAlone() = runTest {
        val api = Api(); val billing = Billing()
        billing.owned = listOf(
            PlayPurchase("paid-while-closed", listOf(starter.googleProductId), "acct-hash", false),
            PlayPurchase("handle", listOf("pigeonpost.handle.01"), "acct-hash", false),
        )
        val store = NestStore(api, billing, backgroundScope)
        store.load(); runCurrent()
        assertEquals(listOf("paid-while-closed"), api.claimed)
        assertEquals(starter.code, store.state.value.funding?.`package`)
    }

    @Test fun pendingAndForeignPurchasesGrantNothing() = runTest {
        val api = Api(); val billing = Billing()
        billing.owned = listOf(
            PlayPurchase("pending", listOf(starter.googleProductId), "acct-hash", true),
            PlayPurchase("someone-else", listOf(plus.googleProductId), "other-hash", false),
        )
        val store = NestStore(api, billing, backgroundScope)
        store.load(); runCurrent()
        assertTrue(api.claimed.isEmpty())
        assertNotNull(store.state.value.error)
    }

    @Test fun aServerRefusalNeverOpensPlay() = runTest {
        val api = Api().apply { intentRefusal = ApiException(409, "nest_package_active") }
        val billing = Billing(); val store = NestStore(api, billing, backgroundScope)
        store.load(); runCurrent(); store.buy(plus); runCurrent()
        assertTrue(events.none { it.startsWith("launch") })
        assertEquals(NestRules.explain("nest_package_active", null), store.state.value.error)
        assertNull(store.state.value.awaitingPayment)
    }

    @Test fun productsPlayDoesNotListAreNotForSale() = runTest {
        val billing = Billing().apply { available = false }
        val store = NestStore(Api(), billing, backgroundScope)
        store.load(); runCurrent()
        assertTrue(store.state.value.playUnavailable)
        assertFalse(store.state.value.canBuy(starter))
        assertFalse(store.state.value.sellable.any { it.code == pro.code })
    }

    @Test fun anUpgradeReplacesTheOwnedSubscriptionOnTheSameRail() = runTest {
        val api = Api(); val billing = Billing()
        billing.owned = listOf(PlayPurchase("tok-starter", listOf(starter.googleProductId), "acct-hash", false))
        val store = NestStore(api, billing, backgroundScope)
        store.load(); runCurrent(); events.clear()
        store.buy(plus); runCurrent()
        assertEquals(listOf("intent:${plus.code}:google", "replace:${plus.googleProductId}:tok-starter:true"), events)
    }

    @Test fun approvalsSendTheHashThatWasShownAndOnlyForPendingCards() = runTest {
        val api = Api(); val store = NestStore(api, null, backgroundScope)
        store.open("nst_00ab"); runCurrent()
        val cards = store.state.value.detail!!.approvals
        store.decide(cards.first { it.status == "superseded" }, true); runCurrent()
        assertNull(api.decided)
        store.decide(cards.first { it.status == "pending" }, true); runCurrent()
        assertEquals(Triple("nap_1", true, "hash-nap_1"), api.decided)
    }

    @Test fun uploadsAreBoundedAndDeletionNeedsTheExactName() = runTest {
        val api = Api(); val store = NestStore(api, null, backgroundScope)
        store.open("nst_00ab"); runCurrent()
        store.upload("big.bin", ByteArray((NestRules.MAX_UPLOAD_BYTES + 1).toInt())); runCurrent()
        assertTrue(events.none { it.startsWith("upload") })
        store.upload("notes.txt", "hello".toByteArray()); runCurrent()
        assertTrue("upload:notes.txt:5" in events)
        store.delete("research"); runCurrent()
        assertTrue(events.none { it.startsWith("delete") })
        store.delete("Research"); runCurrent()
        assertTrue("delete:Research" in events)
        assertNull(store.state.value.detail)
    }

    @Test fun fileNamesTravelPercentEncodedAndNeverAsPaths() {
        assertEquals("Q3%20plan%2F..%2F%C3%BC.pdf", NestRules.encodeFileName("Q3 plan/../ü.pdf"))
        assertEquals("ab", NestRules.encodeFileName("a\u0000b\n"))
        assertEquals("file", NestRules.encodeFileName("\u0007"))
        assertEquals(200, NestRules.encodeFileName("a".repeat(500)).length)
        assertFalse(NestRules.validNestId("nst_../x")); assertTrue(NestRules.validNestId("nst_0aF".lowercase()))
    }

    @Test fun theClientSendsRawBytesWithTheEncodedNameAndClaimsWithTheToken() = runBlocking {
        val server = MockWebServer(); server.start()
        try {
            val tokens = object : TokenProvider { override suspend fun token(rejected: String?) = "member" }
            val client = PostboxClient(tokens, server.url("/"), allowLoopbackForTests = true)
            server.enqueue(MockResponse().setBody("""{"file_id":"nfl_0123456789abcdef01234567","name":"ü.txt","bytes":2,"status":"parsed"}"""))
            val file = withTimeout(5_000) { client.uploadNestFile("nst_00ab", "ü.txt", byteArrayOf(1, 2)) }
            assertEquals("parsed", file.status)
            val up = server.takeRequest()
            assertEquals("POST", up.method); assertEquals("/v1/nests/nst_00ab/files", up.path)
            assertEquals("%C3%BC.txt", up.getHeader("x-file-name"))
            assertEquals("application/octet-stream", up.getHeader("Content-Type"))
            assertArrayEquals(byteArrayOf(1, 2), up.body.readByteArray())
            server.enqueue(MockResponse().setBody("""{"allowance":{"slots":1,"used":0},"nests":[],"setups":[]}"""))
            withTimeout(5_000) { client.claimNestGoogle("tok") }
            val claim = server.takeRequest()
            assertEquals("/v1/nest-purchases/google", claim.path)
            assertEquals("""{"purchase_token":"tok"}""", claim.body.readUtf8())
            server.enqueue(MockResponse().setBody("{}"))
            withTimeout(5_000) { client.decideNestApproval("nst_00ab", "nap_1", false, "h") }
            val decide = server.takeRequest()
            assertEquals("/v1/nests/nst_00ab/approvals/nap_1/reject", decide.path)
            assertEquals("""{"args_hash":"h"}""", decide.body.readUtf8())
            try { client.nest("nst_../../x"); fail("bad id") } catch (_: IllegalArgumentException) {}
        } finally { server.shutdown() }
    }
}
