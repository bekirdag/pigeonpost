package dev.pigeonpost.core

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.TimeoutCancellationException
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch
import kotlinx.coroutines.withTimeout
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable

@Serializable
data class AccountHandle(val namespace: String, val source: String,
    @SerialName("expires_at") val expiresAt: Long? = null, val active: Boolean) {
    val name get() = "/" + namespace.trimStart('/')
    val provider get() = when (source) { "apple" -> "App Store"; "google" -> "Google Play"; else -> "Pigeonpost" }
}
@Serializable
data class AccountHandlesResponse(val handles: List<AccountHandle>)
interface AccountHandleApi { suspend fun accountHandles(): List<AccountHandle> }
data class AccountHandleState(val handles: List<AccountHandle> = emptyList(), val loaded: Boolean = false,
    val loading: Boolean = false, val error: String? = null)

/** Read account ownership independently of Google Play availability or the selected mailbox. */
class AccountHandleStore(private val api: AccountHandleApi, private val scope: CoroutineScope,
    private val onSessionExpired: () -> Unit = {}) {
    private val mutable = MutableStateFlow(AccountHandleState())
    val state = mutable.asStateFlow()
    private var epoch = 0L
    private var job: Job? = null
    fun reset() { ++epoch; job?.cancel(); job = null; mutable.value = AccountHandleState() }
    fun refresh() {
        if (state.value.loading) return
        val generation = ++epoch
        mutable.update { it.copy(loading = true, error = null) }
        job = scope.launch {
            try {
                val rows = withTimeout(20_000) { api.accountHandles() }
                if (generation == epoch) mutable.value = AccountHandleState(handles = rows, loaded = true)
            } catch (expired: SessionExpired) {
                if (generation == epoch) { reset(); onSessionExpired() }
            } catch (_: TimeoutCancellationException) {
                if (generation == epoch) mutable.update { it.copy(error = "Could not refresh your account handles. Try Refresh again.") }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) {
                if (generation == epoch) mutable.update { it.copy(error = "Could not refresh your account handles. Your registrations are saved. Try Refresh again.") }
            } finally { if (generation == epoch) mutable.update { it.copy(loading = false) } }
        }
    }
}
