@file:OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
package dev.pigeonpost.core

import kotlinx.coroutines.*
import kotlinx.coroutines.test.*
import org.junit.Assert.*
import org.junit.Test
import java.io.IOException

class HandleStoreTest {
    private open class TesterApi : FakePostbox() {
        var held = HandleOffer(eligible = true)
        var claims = 0
        override suspend fun handleOffer() = held
        override suspend fun claimHandle(name: String): HandleOffer {
            claims++
            held = super.claimHandle(name)
            return held
        }
    }
    @Test fun registrationNeedsServerEligibilityAndAnUnchangedAvailableName() = runTest {
        val api = TesterApi(); val store = HandleStore(api, this)
        store.edit(" /Alex/ "); store.register(); advanceUntilIdle(); assertEquals(0, api.claims)
        store.refresh(); advanceUntilIdle(); store.check(); advanceUntilIdle()
        assertEquals("alex", store.state.value.wantedName); assertTrue(store.state.value.canRegister)
        store.edit("blake"); assertFalse(store.state.value.canRegister)
        api.held = HandleOffer(eligible = false); store.refresh(); advanceUntilIdle(); store.check(); advanceUntilIdle()
        store.register(); advanceUntilIdle(); assertEquals(0, api.claims)
    }
    @Test fun aLateAvailabilityResponseCannotEnableADifferentName() = runTest {
        val waiting = CompletableDeferred<HandleAvailability>()
        val api = object : TesterApi() { override suspend fun checkHandle(name: String) = withContext(NonCancellable) { waiting.await() } }
        val store = HandleStore(api, this); store.refresh(); advanceUntilIdle()
        store.edit("alex"); store.check(); runCurrent(); store.edit("blake")
        waiting.complete(HandleAvailability("alex", true)); advanceUntilIdle()
        assertEquals("blake", store.state.value.wantedName); assertNull(store.state.value.availability); assertFalse(store.state.value.canRegister)
    }
    @Test fun doubleTapGrantsOnceAndReloadsTheAccountAfterConfirmation() = runTest {
        val api = TesterApi(); var registered: String? = null
        val store = HandleStore(api, this, { registered = it })
        store.refresh(); advanceUntilIdle(); store.edit("Alex"); store.check(); advanceUntilIdle()
        store.register(); store.register(); advanceUntilIdle()
        assertEquals(1, api.claims); assertEquals("/alex", registered)
        assertEquals("/alex/main", store.state.value.offer?.mailbox); assertFalse(store.state.value.canRegister)
    }
    @Test fun signOutRejectsLateClaimsAndNeverRepairsAnotherAccountsInbox() = runTest {
        val waiting = CompletableDeferred<HandleOffer>()
        var created = false; var notified = false
        val api = object : TesterApi() {
            override suspend fun claimHandle(name: String) = withContext(NonCancellable) { waiting.await() }
            override suspend fun createIdentity(handle: String?): String { created = true; return "/k/new" }
        }
        val store = HandleStore(api, this, { notified = true }); store.refresh(); advanceUntilIdle()
        store.edit("alex"); store.check(); advanceUntilIdle(); store.register(); runCurrent(); store.reset()
        waiting.complete(HandleOffer("/alex", eligible = true)); advanceUntilIdle()
        assertFalse(created); assertFalse(notified); assertEquals(HandleState(), store.state.value)
    }
    @Test fun uncertainRegistrationIsRecoveredFromTheServerWithoutAnotherClaim() = runTest {
        val api = object : TesterApi() { override suspend fun claimHandle(name: String): HandleOffer {
            super.claimHandle(name); throw IOException("response lost")
        } }
        val store = HandleStore(api, this); store.refresh(); advanceUntilIdle(); store.edit("alex"); store.check(); advanceUntilIdle()
        store.register(); advanceUntilIdle(); assertNotNull(store.state.value.error)
        store.refresh(); advanceUntilIdle(); assertEquals("/alex", store.state.value.offer?.namespace)
        store.register(); advanceUntilIdle(); assertEquals(1, api.claims)
    }
    @Test fun failedInboxMintKeepsOwnershipAndCanBeRepaired() = runTest {
        var failMint = true
        val api = object : TesterApi() {
            override suspend fun claimHandle(name: String): HandleOffer { held = HandleOffer("/$name", eligible = true); return held }
            override suspend fun createIdentity(handle: String?): String {
                if (failMint) throw IOException("offline")
                held = held.copy(mailbox = handle); return "/k/new"
            }
        }
        val store = HandleStore(api, this); store.refresh(); advanceUntilIdle(); store.edit("alex"); store.check(); advanceUntilIdle()
        store.register(); advanceUntilIdle(); assertEquals("/alex", store.state.value.offer?.namespace); assertNotNull(store.state.value.error)
        failMint = false; store.repairInbox(); advanceUntilIdle()
        assertEquals("/alex/main", store.state.value.offer?.mailbox); assertNull(store.state.value.error)
    }
    @Test fun slowLookupsStopShowingProgressAndExpiredSessionsReset() = runTest {
        var expire = false; var signedOut = false
        val api = object : TesterApi() { override suspend fun handleOffer(): HandleOffer {
            if (expire) throw SessionExpired()
            delay(60_000); return super.handleOffer()
        } }
        val store = HandleStore(api, this, onSessionExpired = { signedOut = true })
        store.refresh(); advanceUntilIdle(); assertFalse(store.state.value.busy); assertNotNull(store.state.value.error)
        expire = true; store.refresh(); advanceUntilIdle(); assertTrue(signedOut); assertEquals(HandleState(), store.state.value)
    }
    @Test fun namesRespectTheServerNamespaceShape() {
        for (name in listOf(" /Alex/ ", "a.b_c-d", "a".repeat(32))) assertTrue(name, validHandleName(name))
        for (name in listOf("", "/a/b", "-alex", "alex-", "a b", "özge", "a".repeat(33), "k", "GH")) assertFalse(name, validHandleName(name))
    }
}
