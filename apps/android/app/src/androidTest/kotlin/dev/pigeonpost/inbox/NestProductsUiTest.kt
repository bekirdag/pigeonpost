package dev.pigeonpost.inbox

import android.content.Context
import android.content.Intent
import androidx.activity.compose.setContent
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.pigeonpost.core.*
import dev.pigeonpost.core.nests.*
import dev.pigeonpost.inbox.ui.NestStatusBar
import dev.pigeonpost.inbox.ui.PigeonpostTheme
import dev.pigeonpost.inbox.ui.ProductsDialog
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import kotlinx.serialization.json.putJsonObject
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

/** Nests B2–B5 on Android: the three Products tabs, Play-priced packages, a Nest's page. */
@RunWith(AndroidJUnit4::class)
class NestProductsUiTest {
    @get:Rule val ui = createEmptyComposeRule()
    private val starter = NestPackage("nest.ai.starter.monthly", "Pigeonpost AI", 2, 1, sellable = true, googleProductId = "pigeonpost.nests.ai.starter")
    private val host = NestPackage("nest.host.starter.monthly", "Empty Nest", 1, 1, model = "none", sellable = true, googleProductId = "pigeonpost.nests.host.starter")
    private inner class Api : NestApi {
        var decided: String? = null
        override suspend fun nestPackages() = NestCatalog("v", listOf(starter, host), NestRails(google = true), "acct")
        override suspend fun nests() = NestSnapshot(nests = listOf(NestView("nst_00aa", "/k/nest", "Research", "running", "package", "ready")))
        override suspend fun nest(id: String) = NestView(id, "/k/nest", "Research", "running", "package", "working")
        override suspend fun nestIntent(packageCode: String, rail: String) = NestIntent("npi", starter.googleProductId, "acct")
        override suspend fun claimNestGoogle(purchaseToken: String) = nests()
        override suspend fun retryNestSetup(setupId: String) = nests()
        override suspend fun nestFiles(id: String) = NestFiles(listOf(NestFile("nfl_1", "plan.pdf", 10, "pdf", "partial", coverage = NestCoverage(4, 3))), 10, 100)
        override suspend fun uploadNestFile(id: String, name: String, bytes: ByteArray) = NestFile("nfl_2", name)
        override suspend fun deleteNestFile(id: String, fileId: String) {}
        override suspend fun nestApprovals(id: String) = NestApprovals(listOf(NestApproval("nap_1", "send_message",
            buildJsonObject { put("tool", "send_message"); putJsonObject("args") { put("to", "/bob"); put("body", "Meeting moved to 3pm") } }, "h1", "pending")))
        override suspend fun decideNestApproval(id: String, approvalId: String, approve: Boolean, argsHash: String) { decided = "$approvalId:$approve:$argsHash" }
        override suspend fun nestUsage(id: String) = NestUsage()
        override suspend fun nestAction(id: String, action: String) {}
        override suspend fun renameNest(id: String, label: String) {}
        override suspend fun deleteNest(id: String, confirm: String) {}
        override suspend fun exportNest(id: String, out: java.io.OutputStream) {}
        override suspend fun createNest(label: String) = NestCreated("nst_00bb", "/k/two", "running")
        override suspend fun createOrgNest(orgId: String, handle: String, label: String) = NestCreated("nst_00cc", "/wodo/hr", "running")
        override suspend fun nestOrganizations() = emptyList<OrgSummary>()
        override suspend fun orgNests(orgId: String) = OrgNests()
    }
    private inner class Billing : PlayBilling {
        override val updates = MutableSharedFlow<PlayUpdate>()
        var launches = 0
        override suspend fun prices(products: List<String>, basePlan: String) = emptyList<PlayPrice>()
        // Play lists only the starter package (the others are "not yet in Play").
        override suspend fun subscriptionPrices(products: List<String>, basePlan: String, period: String) =
            listOf(PlayPrice(starter.googleProductId, "€9.99", "EUR", 9_990_000))
        override suspend fun purchases() = emptyList<PlayPurchase>()
        override fun launch(productId: String, accountId: String) { launches++ }
    }
    @Test fun aNestConversationHeaderShowsStatusAndLeadsToItsApprovals() {
        var opened: String? = null
        val context = ApplicationProvider.getApplicationContext<Context>()
        ActivityScenario.launch<MainActivity>(Intent(context, MainActivity::class.java).putExtra("pigeonpost.fixtures", "inbox")).use { scenario ->
            scenario.onActivity { activity -> activity.setContent {
                val scope = rememberCoroutineScope()
                val store = remember { NestStore(Api(), null, scope) }
                LaunchedEffect(Unit) { store.load() }
                PigeonpostTheme { NestStatusBar(store, "/k/nest") { opened = it } }
            } }
            ui.waitUntil(5_000) { ui.onAllNodesWithText("Review").fetchSemanticsNodes().isNotEmpty() }
            ui.onNodeWithText("Nest · Working… · 1 action to approve").assertExists()
            ui.onNodeWithText("Review").performClick()
            ui.waitUntil(5_000) { opened != null }
            assertEquals("nst_00aa", opened)
        }
    }

    private fun show(api: Api, billing: Billing): ActivityScenario<MainActivity> {
        val context = ApplicationProvider.getApplicationContext<Context>()
        return ActivityScenario.launch<MainActivity>(Intent(context, MainActivity::class.java).putExtra("pigeonpost.fixtures", "inbox")).also { scenario ->
            scenario.onActivity { activity -> activity.setContent {
                val scope = rememberCoroutineScope()
                val store = remember { NestStore(api, billing, scope) }
                PigeonpostTheme { ProductsDialog(store, {}, {}, {}, {}, {}, {}) }
            } }
        }
    }

    @Test fun threeTabsPlayPricesAndPackagesPlayDoesNotListAreNotBuyable() {
        val billing = Billing()
        show(Api(), billing).use {
            ui.onNodeWithText("Buy an Inbox").assertExists()
            ui.onNodeWithText("Services").assertExists()
            ui.waitUntil(5_000) { ui.onAllNodesWithText("€9.99 / month").fetchSemanticsNodes().isNotEmpty() }
            ui.onNodeWithTag("package-${host.code}").assertExists()
            ui.onAllNodesWithText("Not available").assertCountEquals(1)
            ui.onNodeWithContentDescription("Buy Pigeonpost AI").assertIsEnabled().performClick()
            ui.waitUntil(5_000) { billing.launches == 1 }
            ui.onNodeWithText("Services").performClick()
            ui.onAllNodesWithText("Coming soon").assertCountEquals(5)
        }
    }

    @Test fun aNestPageShowsStatusFilesAndTheExactActionToApprove() {
        val api = Api()
        show(api, Billing()).use {
            ui.waitUntil(5_000) { ui.onAllNodesWithTag("openNest").fetchSemanticsNodes().isNotEmpty() }
            ui.onNodeWithTag("openNest").performClick()
            ui.waitUntil(5_000) { ui.onAllNodesWithTag("nestStatus").fetchSemanticsNodes().isNotEmpty() }
            ui.onNodeWithText("Working…").assertExists()
            ui.onNodeWithText("plan.pdf").assertExists()
            ui.onNodeWithText("Send a message to /bob").performScrollTo().assertExists()
            ui.onNodeWithText("Meeting moved to 3pm").assertExists()
            ui.onNodeWithText("Check code h1").performScrollTo().assertExists()
            ui.onNodeWithText("Approve").performScrollTo().performClick()
            ui.waitUntil(5_000) { api.decided != null }
            assertEquals("nap_1:true:h1", api.decided)
            ui.onNodeWithTag("deleteNest").performScrollTo().assertIsNotEnabled()
        }
    }
}
