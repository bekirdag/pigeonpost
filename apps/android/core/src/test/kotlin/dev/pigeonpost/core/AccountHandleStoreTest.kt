@file:OptIn(kotlinx.coroutines.ExperimentalCoroutinesApi::class)
package dev.pigeonpost.core

import kotlinx.coroutines.*
import kotlinx.coroutines.test.*
import org.junit.Assert.*
import org.junit.Test
import java.io.IOException

class AccountHandleStoreTest {
    private val rows = listOf(AccountHandle("apple", "apple", 300, true), AccountHandle("google", "google", 200, false), AccountHandle("web", "entitlement", null, true))
    @Test fun ownershipDoesNotDependOnPlayOrAnActiveSubscription() = runTest {
        var fail = false
        val api = object : AccountHandleApi { override suspend fun accountHandles(): List<AccountHandle> { if (fail) throw IOException(); return rows } }
        val store = AccountHandleStore(api, this)
        store.refresh(); advanceUntilIdle()
        assertEquals(rows, store.state.value.handles); assertTrue(store.state.value.loaded)
        assertEquals("Google Play", store.state.value.handles[1].provider)
        assertFalse(store.state.value.handles[1].active)
        fail = true; store.refresh(); advanceUntilIdle()
        assertEquals(rows, store.state.value.handles); assertNotNull(store.state.value.error)
        fail = false; store.refresh(); advanceUntilIdle(); assertNull(store.state.value.error)
    }
    @Test fun oldAccountResponseCannotRepopulateAfterSignOut() = runTest {
        val waiting = CompletableDeferred<List<AccountHandle>>()
        val api = object : AccountHandleApi { override suspend fun accountHandles() = withContext(NonCancellable) { waiting.await() } }
        val store = AccountHandleStore(api, this)
        store.refresh(); runCurrent(); store.reset(); waiting.complete(rows); advanceUntilIdle()
        assertEquals(AccountHandleState(), store.state.value)
    }
    @Test fun timeoutIsVisibleAndExpiredSessionIsCleared() = runTest {
        var expire = false; var signedOut = false
        val api = object : AccountHandleApi { override suspend fun accountHandles(): List<AccountHandle> { if (expire) throw SessionExpired(); delay(60_000); return rows } }
        val store = AccountHandleStore(api, this) { signedOut = true }
        store.refresh(); advanceUntilIdle()
        assertFalse(store.state.value.loading); assertNotNull(store.state.value.error)
        expire = true; store.refresh(); advanceUntilIdle()
        assertTrue(signedOut); assertEquals(AccountHandleState(), store.state.value)
    }
}
