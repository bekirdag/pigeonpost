package dev.pigeonpost.core.enterprise

import dev.pigeonpost.core.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import java.time.Instant
import java.util.UUID

class DeviceApprovalIdentity(val subject: String, val authorization: AuthorizationLease) {
    fun same(other: DeviceApprovalIdentity?) = other != null && subject == other.subject && authorization === other.authorization
    override fun toString() = "DeviceApprovalIdentity(redacted)"
}
enum class DeviceApprovalPhase { IDLE, RESOLVING, REVIEW, APPROVING, DENYING, VERIFYING, APPROVED, DENIED, EXPIRED, UNCONFIRMED, UNAVAILABLE }
data class DeviceApprovalState(
    val contextId: String = UUID.randomUUID().toString(), val authorizationId: String? = null,
    val proofId: String = UUID.randomUUID().toString(), val code: DevicePairingCode? = null, val preview: DevicePairingPreview? = null,
    val phase: DeviceApprovalPhase = DeviceApprovalPhase.IDLE, val challenge: StrongSignInRequirement? = null,
    val message: String? = null, val retryAt: Long? = null,
) {
    val busy get() = phase in setOf(DeviceApprovalPhase.RESOLVING, DeviceApprovalPhase.APPROVING, DeviceApprovalPhase.DENYING, DeviceApprovalPhase.VERIFYING)
}

/** Human account scope, independent of the currently selected inbox. No decision runs on arrival. */
class DeviceApprovalStore(
    private val api: DevicePairingApi, private val scope: CoroutineScope,
    private val identity: () -> DeviceApprovalIdentity?, private val transitioning: () -> Boolean = { false },
    private val now: () -> Long = { Instant.now().epochSecond },
    private val onSessionExpired: (AuthorizationLease) -> Unit = {},
) {
    private val mutable = MutableStateFlow(DeviceApprovalState())
    val state = mutable.asStateFlow()
    private var bound: DeviceApprovalIdentity? = null
    private var actor: AuthorizationLease? = null
    private var request: AuthorizationLease? = null
    private var work: Job? = null
    private var operation = UUID.randomUUID()
    private var rateSubject: String? = null
    private var deadline: Long? = null
    private fun liveIdentity() = identity()?.takeIf { it.subject.isNotEmpty() && it.subject.toByteArray().size <= 256 && it.authorization.isValid }
    fun isCurrent(context: String = state.value.contextId): Boolean = actor?.isValid == true && context == state.value.contextId && bound?.same(liveIdentity()) == true
    private fun current(owner: AuthorizationLease, visit: UUID) = actor === owner && operation == visit && isCurrent(owner.id)
    private fun stop() {
        operation = UUID.randomUUID(); request?.retire(); request = null; work?.cancel(); work = null
    }
    private fun bind(next: DeviceApprovalIdentity?, preserveRate: Boolean = false) {
        stop(); actor?.retire(); actor = null; bound = null
        if (!preserveRate && next?.subject != rateSubject) { rateSubject = null; deadline = null }
        val child = try { next?.authorization?.child() } catch (_: AuthorizationRetired) { null }
        if (child != null) { actor = child; bound = next }
        mutable.value = DeviceApprovalState(contextId = child?.id ?: UUID.randomUUID().toString(), authorizationId = if (child != null) next?.authorization?.id else null, retryAt = deadline)
    }
    fun synchronize() {
        val next = liveIdentity()
        if (bound?.same(next) == true && actor?.isValid == true) return
        // Only this awaited verification may bridge retirement and adopt its returned grant.
        if (state.value.phase == DeviceApprovalPhase.VERIFYING && bound != null &&
            (next == null && transitioning() || next?.subject == bound?.subject)) return
        bind(next)
    }
    fun close() { bind(null, preserveRate = liveIdentity()?.subject == rateSubject) }
    fun changeCode(expectedContext: String) {
        if (!isCurrent(expectedContext) || state.value.busy) return
        stop()
        mutable.value = DeviceApprovalState(contextId = state.value.contextId, authorizationId = state.value.authorizationId, retryAt = deadline)
    }
    fun refreshExpiry() {
        if (deadline?.let { it <= now() } == true) { deadline = null; mutable.update { it.copy(retryAt = null) } }
        if (state.value.phase == DeviceApprovalPhase.REVIEW && state.value.preview?.expiresAt?.let { it <= now() } == true)
            mutable.update { it.copy(proofId = UUID.randomUUID().toString(), phase = DeviceApprovalPhase.EXPIRED, challenge = null, message = "This request expired. Start again on the machine.") }
    }
    fun canApprove(proof: String, context: String): Boolean = isCurrent(context) && state.value.proofId == proof &&
        state.value.phase == DeviceApprovalPhase.REVIEW && state.value.challenge == null && state.value.preview?.allowsApproval(now()) == true
    fun canDeny(proof: String, context: String): Boolean = isCurrent(context) && state.value.proofId == proof &&
        state.value.phase == DeviceApprovalPhase.REVIEW && state.value.preview?.allowsDecision(now()) == true && state.value.preview?.canApprove == true
    private fun limited(): Boolean {
        if (deadline?.let { it > now() } != true) return false
        mutable.update { it.copy(message = "Too many attempts. Wait before checking another request.", retryAt = deadline) }; return true
    }
    private fun run(phase: DeviceApprovalPhase, context: String, uncertain: Boolean = false, action: suspend (AuthorizationLease, UUID) -> Unit): Job? {
        synchronize()
        val owner = actor ?: return null
        if (!isCurrent(context) || state.value.busy || limited()) return null
        val child = try { owner.child() } catch (_: AuthorizationRetired) { synchronize(); return null }
        stop(); val visit = UUID.randomUUID(); operation = visit; request = child
        mutable.update { it.copy(phase = phase, message = null, proofId = UUID.randomUUID().toString()) }
        val job = scope.launch(AuthorizationContext(child), start = CoroutineStart.LAZY) {
            try { child.requireActive(); action(owner, visit) }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: Exception) { if (current(owner, visit)) failed(failure, uncertain) }
            finally {
                child.retire()
                if (current(owner, visit) && state.value.phase == phase)
                    mutable.update { it.copy(phase = if (uncertain) DeviceApprovalPhase.UNCONFIRMED else DeviceApprovalPhase.IDLE) }
            }
        }
        work = job; job.start(); return job
    }
    fun resolve(input: String, expectedContext: String): Job? {
        synchronize()
        if (!isCurrent(expectedContext) || state.value.busy) return null
        val code = DevicePairingCode.parse(input)
        if (code == null) { mutable.update { it.copy(message = "Enter the eight-character code shown on the machine.") }; return null }
        return run(DeviceApprovalPhase.RESOLVING, expectedContext) { owner, visit ->
            mutable.update { it.copy(code = code, preview = null, challenge = null) }
            val loaded = api.resolveDevicePairing(code)
            if (current(owner, visit)) apply(loaded)
        }
    }
    fun checkStatus(expectedContext: String) = state.value.code?.let { resolve(it.displayValue, expectedContext) }
    private fun apply(loaded: DevicePairingPreview) {
        if (!loaded.isValid) throw ApiException(200, "bad_response")
        val phase = when {
            loaded.state == "approved" -> DeviceApprovalPhase.APPROVED
            loaded.state == "denied" -> DeviceApprovalPhase.DENIED
            loaded.expiresAt <= now() -> DeviceApprovalPhase.EXPIRED
            else -> DeviceApprovalPhase.REVIEW
        }
        val message = when {
            phase == DeviceApprovalPhase.EXPIRED -> "This request expired. Start again on the machine."
            !loaded.canApprove && phase == DeviceApprovalPhase.REVIEW -> if (loaded.reason == "delegation_required") "Ask an agent admin to add you as an operator." else "Your account cannot decide this request."
            loaded.capabilities != listOf("mailbox") && phase == DeviceApprovalPhase.REVIEW -> "These permissions are not supported by this app. Ask your administrator."
            else -> null
        }
        mutable.update { it.copy(preview = loaded, phase = phase, proofId = UUID.randomUUID().toString(), challenge = null, message = message) }
    }
    fun approve(proof: String, context: String): Job? {
        refreshExpiry()
        if (!canApprove(proof, context)) return null
        val preview = state.value.preview!!; val code = state.value.code!!
        return run(DeviceApprovalPhase.APPROVING, context, uncertain = true) { owner, visit ->
            api.approveDevicePairing(preview, code)
            if (current(owner, visit)) mutable.update { it.copy(phase = DeviceApprovalPhase.APPROVED, challenge = null) }
        }
    }
    fun deny(proof: String, context: String): Job? {
        refreshExpiry()
        if (!canDeny(proof, context)) return null
        val preview = state.value.preview!!; val code = state.value.code!!
        return run(DeviceApprovalPhase.DENYING, context, uncertain = true) { owner, visit ->
            api.denyDevicePairing(preview, code)
            val loaded = api.resolveDevicePairing(code)
            if (current(owner, visit)) {
                if (loaded.state in setOf("approved", "denied")) apply(loaded)
                else mutable.update { it.copy(phase = DeviceApprovalPhase.UNCONFIRMED, message = "The denial was sent, but the final status is not confirmed. Check it again.") }
            }
        }
    }
    fun verifySignIn(proof: String, context: String, verify: suspend (StrongSignInRequirement, AuthorizationLease) -> AuthorizationLease): Job? {
        refreshExpiry()
        if (!isCurrent(context) || state.value.phase != DeviceApprovalPhase.REVIEW || state.value.proofId != proof || limited()) return null
        val requirement = state.value.challenge ?: return null
        val original = bound ?: return null
        val code = state.value.code ?: return null
        stop(); val visit = UUID.randomUUID(); operation = visit
        mutable.update { it.copy(phase = DeviceApprovalPhase.VERIFYING, proofId = UUID.randomUUID().toString(), message = null) }
        // Deliberately outside the old actor context: the verified interaction retires that root.
        val job = scope.launch(start = CoroutineStart.LAZY) {
            try {
                val accepted = verify(requirement, original.authorization)
                if (operation != visit) return@launch
                val next = liveIdentity()
                if (next == null || next.subject != original.subject || next.authorization !== accepted || accepted === original.authorization || !accepted.isValid) {
                    work = null; bind(next); return@launch
                }
                work = null; bind(next)
                resolve(code.displayValue, state.value.contextId) // A read, followed by another explicit human decision.
            } catch (cancelled: CancellationException) {
                if (operation == visit) { mutable.update { it.copy(phase = DeviceApprovalPhase.REVIEW, message = "Verification was cancelled.") }; synchronize() }
                throw cancelled
            } catch (_: Exception) {
                if (operation == visit) { mutable.update { it.copy(phase = DeviceApprovalPhase.REVIEW, message = "Sign-in was not verified. Try again with the same account.") }; synchronize() }
            }
        }
        work = job; job.start(); return job
    }
    private fun failed(failure: Exception, uncertain: Boolean) {
        val api = failure as? ApiException
        if (failure is SessionExpired) { val accepted = bound?.authorization; bind(null); accepted?.let(onSessionExpired); return }
        if (api?.needsStrongSignIn == true) {
            mutable.update { it.copy(phase = if (api.signInRequirement == null) DeviceApprovalPhase.UNAVAILABLE else DeviceApprovalPhase.REVIEW,
                proofId = UUID.randomUUID().toString(), challenge = api.signInRequirement,
                message = if (api.signInRequirement == null) "The server could not provide a supported verification method." else "Verify your sign-in, then review and decide this request again.") }; return
        }
        if (api?.status == 429) { rateSubject = bound?.subject; deadline = now() + (api.retryAfter ?: 30).coerceIn(1, 3600) }
        val phase = when (api?.code) {
            "pairing_expired" -> DeviceApprovalPhase.EXPIRED
            "forbidden", "human_required", "delegation_required", "feature_disabled", "step_up_unavailable", "pairing_not_found" -> DeviceApprovalPhase.UNAVAILABLE
            else -> if (uncertain) DeviceApprovalPhase.UNCONFIRMED else DeviceApprovalPhase.IDLE
        }
        val message = when {
            api?.status == 429 -> "Too many attempts. Wait before checking again."
            api?.code == "pairing_expired" -> "This request expired. Start again on the machine."
            api?.code == "pairing_not_found" -> "No request found for this code. Check it on the machine."
            phase == DeviceApprovalPhase.UNAVAILABLE -> "Your account cannot decide this request, or the feature is unavailable."
            uncertain -> "The decision is not confirmed. Check the request's status before trying again."
            else -> "The request could not be loaded. Try again."
        }
        mutable.update { it.copy(phase = phase, proofId = UUID.randomUUID().toString(), challenge = null, retryAt = deadline, message = message,
            preview = if (api?.status in setOf(403, 404)) null else it.preview) }
    }
}
