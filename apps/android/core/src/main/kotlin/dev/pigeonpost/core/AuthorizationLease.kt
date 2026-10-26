package dev.pigeonpost.core

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.withContext
import kotlin.coroutines.AbstractCoroutineContextElement
import kotlin.coroutines.CoroutineContext
import java.util.UUID
import java.util.concurrent.atomic.AtomicBoolean

/** A retired actor is cancellation, not evidence that the currently signed-in session expired. */
class AuthorizationRetired : CancellationException("This sign-in context is no longer active.")

/**
 * Revocable permission for synchronous local work. Never suspend inside [withActive]. Ancestors
 * are locked before children; retirement is immediate and [awaitQuiescence] waits off the UI
 * thread for previously admitted local work before a replacement grant is published.
 */
class AuthorizationLease(private val parent: AuthorizationLease? = null) {
    val id: String = UUID.randomUUID().toString()
    private val active = AtomicBoolean(true)
    private val monitor = Any()
    val isValid: Boolean get() = active.get() && (parent?.isValid != false)

    fun requireActive() { if (!isValid) throw AuthorizationRetired() }
    fun child(): AuthorizationLease = withActive { AuthorizationLease(this) }
    fun retire() { active.set(false) }
    private fun root(): AuthorizationLease = if (parent == null) this else parent.root()
    fun sharesAuthorization(other: AuthorizationLease): Boolean = root() === other.root()
    fun <T> withActive(work: () -> T): T {
        requireActive() // Retired work must not wait behind an already-admitted local writer.
        val guarded = {
            synchronized(monitor) {
                if (!active.get()) throw AuthorizationRetired()
                work()
            }
        }
        return if (parent != null) parent.withActive(guarded) else guarded()
    }
    suspend fun awaitQuiescence() {
        check(!active.get()) { "Retire this context before waiting for its local operations." }
        withContext(Dispatchers.IO) { synchronized(monitor) { /* Previously admitted operations have finished. */ } }
    }
}

/** Both sides of a suspended acquisition are checked; an actor never borrows a replacement grant. */
class ScopedTokenProvider(
    private val source: TokenProvider,
    override val authorizationLease: AuthorizationLease,
) : TokenProvider {
    private val sourceGrant = source.authorizationLease
    private fun checkCurrent() {
        authorizationLease.requireActive()
        if (source.authorizationLease !== sourceGrant) throw AuthorizationRetired()
        sourceGrant?.let { it.requireActive(); if (!authorizationLease.sharesAuthorization(it)) throw AuthorizationRetired() }
    }
    override suspend fun token(rejected: String?): String {
        checkCurrent()
        val token = source.token(rejected, sourceGrant)
        checkCurrent()
        return token
    }
    override suspend fun invalidate(rejected: String) {
        checkCurrent()
        source.invalidate(rejected, sourceGrant)
        checkCurrent()
    }
}

/** Immutable at coroutine launch: later requests in an old task keep the old authorization. */
class AuthorizationContext(val lease: AuthorizationLease?) : AbstractCoroutineContextElement(Key) {
    companion object Key : CoroutineContext.Key<AuthorizationContext>
}

/** Install only after retiring/resetting stores; closing the child does not revoke the login. */
class AuthorizedStoreScope(private val base: CoroutineScope) : CoroutineScope {
    @Volatile private var active: AuthorizationLease? = null
    override val coroutineContext: CoroutineContext get() = base.coroutineContext + AuthorizationContext(active)
    fun activate(parent: AuthorizationLease?) { active?.retire(); active = parent?.child() }
    fun retire() { active?.retire(); active = null }
}
