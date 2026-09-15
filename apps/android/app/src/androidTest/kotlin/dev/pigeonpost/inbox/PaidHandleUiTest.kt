package dev.pigeonpost.inbox

import android.content.Context
import android.content.Intent
import androidx.activity.compose.setContent
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createEmptyComposeRule
import androidx.test.core.app.ActivityScenario
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.pigeonpost.core.*
import dev.pigeonpost.inbox.ui.PageDialog
import dev.pigeonpost.inbox.ui.PaidHandleSection
import dev.pigeonpost.inbox.ui.PaidHandleManagement
import dev.pigeonpost.inbox.ui.PigeonpostTheme
import kotlinx.coroutines.flow.MutableSharedFlow
import org.junit.Assert.*
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class PaidHandleUiTest {
    @get:Rule val ui = createEmptyComposeRule()
    private val ids = (1..10).map { "pigeonpost.handle.${it.toString().padStart(2, '0')}" }
    private inner class Api : PaidHandleApi {
        var catalog = PlayCatalog(true, ids, "annual", 10, "test-account")
        var assigned = 0
        override suspend fun playCatalog() = catalog
        override suspend fun checkHandle(name: String) = HandleAvailability(name, name != "reserved")
        override suspend fun redeemPlayPurchase(token: String, name: String?) = error("No real purchases in UI tests")
        override suspend fun assignPlayHandle(productId: String, name: String): PlayClaim {
            assigned++
            val purchase = catalog.handles.single().copy(namespace = name)
            catalog = catalog.copy(handles = listOf(purchase))
            return PlayClaim(purchase, "/$name/main")
        }
    }
    private inner class Billing : PlayBilling {
        override val updates = MutableSharedFlow<PlayUpdate>()
        var launches = 0
        override suspend fun prices(products: List<String>, basePlan: String) = products.map { PlayPrice(it, "$8.00", "USD", 8_000_000) }
        override suspend fun purchases() = emptyList<PlayPurchase>()
        override fun launch(productId: String, accountId: String) { launches++ }
    }
    private fun launch(api: Api, billing: Billing, management: Boolean = false): ActivityScenario<MainActivity> {
        val context = ApplicationProvider.getApplicationContext<Context>()
        return ActivityScenario.launch<MainActivity>(Intent(context, MainActivity::class.java).putExtra("pigeonpost.fixtures", "inbox")).also { scenario ->
            scenario.onActivity { activity -> activity.setContent {
                val scope = rememberCoroutineScope()
                val store = remember { PaidHandleStore(api, billing, scope) }
                PigeonpostTheme { PageDialog(if (management) "Your subscriptions" else "Get a handle", {}) {
                    if (management) PaidHandleManagement(store, emptyList(), {}, {}) else PaidHandleSection(store, {})
                } }
            } }
        }
    }
    @Test fun annualPriceAndTenHandleTotalAreVisibleBeforeCheckout() {
        val api = Api(); val billing = Billing()
        launch(api, billing).use {
            ui.onNodeWithText("New handle").performTextInput("alex")
            ui.onNodeWithText("Check availability").performScrollTo().performClick()
            ui.onNodeWithText("/alex is available").assertExists()
            ui.onNodeWithText("All 10 handles: $80.00 per year in total.").performScrollTo().assertIsDisplayed()
            ui.onNodeWithText("Subscribe · $8.00 / year").performScrollTo().assertIsEnabled().performClick()
            ui.runOnIdle { assertEquals(1, billing.launches) }
        }
    }
    @Test fun anAlreadyPaidHandleCanBeNamedWithoutCheckout() {
        val api = Api(); val billing = Billing()
        api.catalog = api.catalog.copy(handles = listOf(PlayHandle(ids[0], null, 2_000_000_000, "SUBSCRIPTION_STATE_ACTIVE", true, true, true, true)))
        launch(api, billing).use {
            ui.onNodeWithText("New handle").performTextInput("alex")
            ui.onNodeWithText("Check availability").performScrollTo().performClick()
            ui.onNodeWithText("Register paid handle").performScrollTo().performClick()
            ui.onNodeWithText("/alex is registered.").assertExists()
            ui.runOnIdle { assertEquals(1, api.assigned); assertEquals(0, billing.launches) }
        }
    }

    @Test fun ownedNamesAndRenewalsStayInManagementRatherThanAcquisition() {
        val api = Api(); val billing = Billing()
        api.catalog = api.catalog.copy(handles = listOf(PlayHandle(ids[0], "studio", 2_000_000_000, "SUBSCRIPTION_STATE_ACTIVE", true, true, true, true)))
        launch(api, billing).use {
            ui.onNodeWithText("New handle").assertExists()
            ui.onNodeWithText("/studio").assertDoesNotExist()
            ui.onNodeWithText("1 of 10 subscriptions active").assertDoesNotExist()
            ui.onNodeWithText("Restore purchases").performScrollTo().assertIsDisplayed()
            ui.onNodeWithText("Terms of service").performScrollTo().assertIsDisplayed()
        }
        launch(api, billing, management = true).use {
            ui.onNodeWithText("/studio").assertExists()
            ui.onNodeWithText("Renews", substring = true).assertExists()
            ui.onNodeWithText("1 of 10 subscriptions active").assertExists()
            ui.onNodeWithText("Manage Google Play subscriptions").performScrollTo().assertIsDisplayed()
            ui.onNodeWithText("New handle").assertDoesNotExist()
        }
    }

    @Test fun tenthHandleLimitKeepsRecoveryAvailableWithoutOwnedRows() {
        val api = Api(); val billing = Billing()
        api.catalog = api.catalog.copy(handles = ids.mapIndexed { i, id -> PlayHandle(id, "owned" + i, 2_000_000_000, "SUBSCRIPTION_STATE_ACTIVE", true, true, true, true) })
        launch(api, billing).use {
            ui.onNodeWithText("New handle").assertDoesNotExist()
            ui.onNodeWithText("/owned0").assertDoesNotExist()
            ui.onNodeWithText("All ten subscriptions are in use. Manage your names under Handles.").performScrollTo().assertIsDisplayed()
            ui.onNodeWithText("Restore purchases").performScrollTo().assertIsEnabled()
            ui.runOnIdle { assertEquals(0, billing.launches) }
        }
    }
}
