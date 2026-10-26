package dev.pigeonpost.core

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
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
