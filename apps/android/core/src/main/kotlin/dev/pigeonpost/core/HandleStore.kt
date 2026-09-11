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

data class HandleState(
    val loading: Boolean = false,
    val loaded: Boolean = false,
    val checking: Boolean = false,
    val registering: Boolean = false,
    val wantedName: String = "",
    val offer: HandleOffer? = null,
    val availability: HandleAvailability? = null,
    val error: String? = null,
) {
    val busy get() = loading || checking || registering
    val canRegister get() = !busy && offer?.eligible == true && offer.namespace == null &&
        validHandleName(wantedName) && availability?.available == true && availability.name == tidyHandle(wantedName)
}

/** Account-scoped registration; server ownership is authoritative, including after an uncertain retry. */
class HandleStore(
    private val api: PostboxApi,
    private val scope: CoroutineScope,
    private val onRegistered: (String) -> Unit = {},
    private val onSessionExpired: () -> Unit = {},
) {
    private val mutable = MutableStateFlow(HandleState())
    val state = mutable.asStateFlow()
    private var epoch = 0L
    private var job: Job? = null

    fun reset() { ++epoch; job?.cancel(); job = null; mutable.value = HandleState() }

    fun edit(raw: String) {
        if (state.value.registering || state.value.loading) return
        ++epoch; job?.cancel()
        mutable.update { it.copy(wantedName = raw.take(80), availability = null, checking = false, error = null) }
    }

    fun refresh() {
        if (state.value.busy) return
        val version = epoch + 1
        run({ it.copy(loading = true, error = null) }) {
            val offer = api.handleOffer()
            if (version != epoch) throw CancellationException()
            offer.namespace?.let(onRegistered)
            return@run { old -> old.copy(offer = offer, loaded = true) }
        }
    }

    fun check() {
        val name = tidyHandle(state.value.wantedName)
        if (state.value.busy || !validHandleName(name)) return
        run({ it.copy(checking = true, wantedName = name, availability = null, error = null) }) {
            val available = api.checkHandle(name)
            return@run { old -> old.copy(availability = available) }
        }
    }

    fun register() {
        if (!state.value.canRegister) return
        val name = tidyHandle(state.value.wantedName)
        val version = epoch + 1
        run({ it.copy(registering = true, error = null) }) {
            val offer = api.claimHandle(name)
            if (version != epoch) throw CancellationException()
            val namespace = offer.namespace ?: throw ApiException(502, "claim_incomplete", "The name was not confirmed. Check again before retrying.")
            // Claims normally mint /name/main. Repair a failed mint without discarding ownership.
            val complete = if (offer.mailbox == null) {
                mutable.update { it.copy(offer = offer) }
                api.createIdentity("${namespace.trimEnd('/')}/main")
                if (version != epoch) throw CancellationException()
                api.handleOffer()
            } else offer
            if (version != epoch) throw CancellationException()
            onRegistered(namespace)
            return@run { old -> old.copy(offer = complete, loaded = true, wantedName = "", availability = null) }
        }
    }

    fun repairInbox() {
        val offer = state.value.offer ?: return
        val namespace = offer.namespace ?: return
        if (state.value.busy || offer.mailbox != null) return
        val version = epoch + 1
        run({ it.copy(registering = true, error = null) }) {
            api.createIdentity("${namespace.trimEnd('/')}/main")
            if (version != epoch) throw CancellationException()
            val complete = api.handleOffer()
            if (version != epoch) throw CancellationException()
            onRegistered(namespace)
            return@run { old -> old.copy(offer = complete) }
        }
    }

    private fun run(start: (HandleState) -> HandleState, work: suspend () -> (HandleState) -> HandleState) {
        val version = ++epoch
        job?.cancel()
        mutable.update(start)
        job = scope.launch {
            try {
                val update = withTimeout(20_000) { work() }
                if (version == epoch) mutable.update(update)
            } catch (_: TimeoutCancellationException) {
                if (version == epoch) mutable.update { it.copy(error = "The postbox took too long. Check again; a completed registration is saved on your account.") }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (_: SessionExpired) { if (version == epoch) { reset(); onSessionExpired() } }
            catch (failure: Exception) {
                if (version == epoch) mutable.update { it.copy(error = explain(failure), availability = null) }
            } finally {
                if (version == epoch) mutable.update { it.copy(loading = false, checking = false, registering = false) }
            }
        }
    }

    private fun explain(failure: Exception) = when ((failure as? ApiException)?.code) {
        "tester_required" -> "Free registration is for approved testers. Sign in with your approved, verified account."
        "namespace_taken" -> "Someone already has that name. Try another."
        "name_reserved" -> "That name is reserved. Try another."
        "tester_already_named" -> "You already registered a free handle. Tap Check again to see it."
        "invalid_namespace" -> "Choose 1–32 letters, numbers, dots, underscores or hyphens."
        "not_found" -> "Handle registration is not available on this postbox yet. Please try again later."
        else -> if (failure is ApiException) failure.message ?: "Could not register that name."
            else "Could not reach the postbox. Your name is kept here; check again to see whether registration finished."
    }
}
