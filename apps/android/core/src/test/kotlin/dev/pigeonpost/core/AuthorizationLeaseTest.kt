package dev.pigeonpost.core

import kotlinx.coroutines.*
import org.junit.Assert.*
import org.junit.Test
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger
import kotlin.concurrent.thread

class AuthorizationLeaseTest {
    @Test fun retiringOneActorKeepsItsSiblingAndParent() {
        val root = AuthorizationLease()
        val first = root.child()
        val other = root.child()
        first.retire()
        assertFalse(first.isValid); assertTrue(root.isValid); assertTrue(other.isValid)
        root.retire()
        assertFalse(other.isValid)
    }
    @Test fun retiredContextsCannotAdmitMoreWorkOrChildren() {
        val root = AuthorizationLease(); root.retire()
        assertThrows(AuthorizationRetired::class.java) { root.withActive { error("must not run") } }
        assertThrows(AuthorizationRetired::class.java) { root.child() }
    }
    @Test fun nullableGuardedWorkRunsExactlyOnce() {
        val root = AuthorizationLease(); val actor = root.child(); var operations = 0
        val result: String? = actor.withActive { operations++; null }
        assertNull(result); assertEquals(1, operations)
    }
    @Test fun parentRetirementWaitsForAnAlreadyAdmittedChildWrite() = runBlocking {
        val root = AuthorizationLease(); val child = root.child()
        val entered = CountDownLatch(1); val release = CountDownLatch(1)
        val writes = AtomicInteger()
        val writer = thread(name = "lease-test-writer") {
            child.withActive { entered.countDown(); check(release.await(3, TimeUnit.SECONDS)); writes.incrementAndGet() }
        }
        try {
            assertTrue(entered.await(3, TimeUnit.SECONDS)); root.retire()
            val waiting = async(Dispatchers.Default) { root.awaitQuiescence(); true }
            assertNull(withTimeoutOrNull(150) { waiting.await() })
            assertThrows(AuthorizationRetired::class.java) { child.withActive { writes.incrementAndGet() } }
            release.countDown()
            assertTrue(withTimeout(3_000) { waiting.await() }); writer.join(1000)
            assertEquals(1, writes.get())
        } finally { release.countDown(); writer.join(1000) }
    }

    private class Source : TokenProvider {
        @Volatile var grant = AuthorizationLease()
        override val authorizationLease get() = grant
        var calls = 0
        var lastRejected: String? = null
        var invalidated: String? = null
        var invalidateGrant: AuthorizationLease? = null
        var issue: suspend () -> String = { "fixture-current" }
        override suspend fun token(rejected: String?): String { calls++; lastRejected = rejected; return issue() }
        override suspend fun invalidate(rejected: String) { invalidated = rejected }
        override suspend fun invalidate(rejected: String, expectedAuthorization: AuthorizationLease?) {
            invalidateGrant = expectedAuthorization
            super<TokenProvider>.invalidate(rejected, expectedAuthorization)
        }
    }
    @Test fun scopedAcquisitionAndInvalidationCarryTheAcceptedGrant() = runBlocking {
        val source = Source(); val scoped = ScopedTokenProvider(source, source.grant.child())
        assertEquals("fixture-current", scoped.token("fixture-rejected"))
        assertEquals("fixture-rejected", source.lastRejected)
        scoped.invalidate("fixture-current")
        assertSame(source.grant, source.invalidateGrant); assertEquals("fixture-current", source.invalidated)
    }
    @Test fun aRetiredActorNeverAsksTheSessionForCredentials() = runBlocking {
        val source = Source(); val actor = source.grant.child(); val scoped = ScopedTokenProvider(source, actor)
        actor.retire()
        assertTrue(runCatching { scoped.token() }.exceptionOrNull() is AuthorizationRetired)
        assertTrue(runCatching { scoped.invalidate("fixture-current") }.exceptionOrNull() is AuthorizationRetired)
        assertEquals(0, source.calls); assertNull(source.invalidated)
    }
    @Test fun aSuspendedAcquisitionCannotReturnAReplacementAccountsToken() = runBlocking {
        val source = Source(); val scoped = ScopedTokenProvider(source, source.grant.child())
        val entered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        source.issue = { entered.complete(Unit); release.await(); "fixture-next-account" }
        val pending = async { runCatching { scoped.token() } }
        entered.await(); source.grant.retire(); source.grant = AuthorizationLease(); release.complete(Unit)
        assertTrue(pending.await().exceptionOrNull() is AuthorizationRetired)
        assertEquals("fixture-next-account", ScopedTokenProvider(source, source.grant.child()).token())
    }
    @Test fun replacingTheGrantIsRefusedEvenIfTheCallerForgotToRetireIt() = runBlocking {
        val source = Source(); val old = source.grant; val scoped = ScopedTokenProvider(source, old.child())
        source.grant = AuthorizationLease()
        assertTrue(old.isValid)
        assertTrue(runCatching { scoped.token() }.exceptionOrNull() is AuthorizationRetired)
        assertEquals(0, source.calls)
    }
}
