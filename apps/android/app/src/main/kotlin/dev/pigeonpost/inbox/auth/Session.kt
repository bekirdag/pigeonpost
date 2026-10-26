package dev.pigeonpost.inbox.auth

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.util.Base64
import dev.pigeonpost.core.SessionExpired
import dev.pigeonpost.core.AuthorizationLease
import dev.pigeonpost.core.AuthorizationRetired
import dev.pigeonpost.core.AuthorizationContext
import dev.pigeonpost.core.TokenProvider
import dev.pigeonpost.core.StrongSignInRequirement
import dev.pigeonpost.core.StrongSignInProof
import dev.pigeonpost.core.StrongSignInException
import dev.pigeonpost.core.await
import dev.pigeonpost.inbox.AppPolicy
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CompletableDeferred
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.NonCancellable
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import kotlinx.coroutines.withTimeout
import net.openid.appauth.AuthState
import net.openid.appauth.AuthorizationException
import net.openid.appauth.AuthorizationRequest
import net.openid.appauth.AuthorizationResponse
import net.openid.appauth.AuthorizationService
import net.openid.appauth.AuthorizationServiceConfiguration
import net.openid.appauth.CodeVerifierUtil
import net.openid.appauth.ResponseTypeValues
import net.openid.appauth.TokenRequest
import net.openid.appauth.TokenResponse
import okhttp3.FormBody
import okhttp3.OkHttpClient
import okhttp3.Request
import org.json.JSONObject
import java.io.IOException
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicInteger
import kotlin.coroutines.resume
import kotlin.coroutines.resumeWithException

object AuthConfig {
    const val ISSUER = "https://auth.pigeonpost.dev/realms/pigeonpost-prod"
    const val CLIENT = "pigeonpost-mobile"
    const val REDIRECT = "dev.pigeonpost.inbox://oauth2redirect"
    const val AUTH = "$ISSUER/protocol/openid-connect/auth"
    const val TOKEN = "$ISSUER/protocol/openid-connect/token"
    const val LOGOUT = "$ISSUER/protocol/openid-connect/logout"
}
data class SessionState(val loading: Boolean = true, val signedIn: Boolean = false, val busy: Boolean = false, val username: String? = null, val error: String? = null, val termsAccepted: Boolean = false, val authorizationId: String? = null, val subject: String? = null)
interface UserSession : TokenProvider {
    val state: StateFlow<SessionState>
    suspend fun begin(provider: String? = null, otherAccount: Boolean = false): Intent?
    suspend fun complete(result: Intent?)
    suspend fun cancel()
    suspend fun acceptTerms()
    suspend fun signOut()
    /** Returns only the exact new grant from a verified same-person interaction. */
    suspend fun verifyStrongSignIn(requirement: StrongSignInRequirement, expectedAuthorization: AuthorizationLease, open: (Intent) -> Unit): AuthorizationLease = throw StrongSignInException()
    /** A delayed cleanup may only sign out the grant that initiated it. */
    suspend fun signOut(expectedAuthorization: AuthorizationLease?) {
        if (authorizationLease !== expectedAuthorization) throw AuthorizationRetired()
        signOut()
    }
}

class Session internal constructor(context: Context, private val service: AuthorizationService) : UserSession {
    constructor(context: Context) : this(context, AuthorizationService(context))
    private val store = SecureStore(context)
    private val mutex = Mutex()
    private val generation = AtomicLong(0)
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var auth: AuthState? = null
    @Volatile private var acceptedAuthorization: AuthorizationLease? = null
    override val authorizationLease: AuthorizationLease? get() = acceptedAuthorization
    private var acceptedTerms: String? = null
    private var pending: AuthorizationRequest? = null
    private var pendingAt = 0L
    private class StrongAttempt(val authorization: AuthorizationLease, val subject: String, val requirement: StrongSignInRequirement, val began: Long, val epoch: Long) {
        val result = CompletableDeferred<AuthorizationLease>()
        // Cancellation and adoption compete at one explicit commit point.
        val stage = AtomicInteger(0) // pending, committing, cancelled, finished
    }
    private var strong: StrongAttempt? = null
    private val mutable = MutableStateFlow(SessionState())
    override val state = mutable.asStateFlow()
    private val initialized = scope.launch {
        mutex.withLock {
            try {
                store.read()?.let { text ->
                    val saved = JSONObject(text)
                    saved.optString("auth").takeIf { it.isNotEmpty() }?.let { auth = AuthState.jsonDeserialize(it) }
                    acceptedTerms = saved.optString("accepted_terms").takeIf { it == AppPolicy.TERMS_VERSION }
                    // A process restore has no live approval waiter. Never reinterpret a strong
                    // callback as ordinary sign-in or revive a previously reviewed request.
                    if (!saved.optBoolean("pending_strong", false)) {
                        saved.optString("pending").takeIf { it.isNotEmpty() }?.let { pending = AuthorizationRequest.jsonDeserialize(it) }
                        pendingAt = saved.optLong("pending_at")
                    }
                    auth?.authorizationServiceConfiguration?.let { require(trusted(it)) }
                }
                if (auth?.isAuthorized == true) acceptedAuthorization = AuthorizationLease()
                publish()
            } catch (_: Exception) {
                auth = null; acceptedTerms = null; pending = null; store.write(null)
                mutable.value = SessionState(loading = false, error = "Your saved session could not be opened. Please sign in again.")
            }
        }
    }

    override suspend fun begin(provider: String?, otherAccount: Boolean): Intent? {
        initialized.join()
        require(provider == null || provider in setOf("google", "github", "apple"))
        val epoch = generation.incrementAndGet()
        mutable.value = mutable.value.copy(busy = true, error = null)
        return try {
            mutex.withLock {
                abortStrong(); pending = null; pendingAt = 0; persist()
                if (epoch != generation.get()) throw CancellationException()
            }
            val config = discover()
            require(trusted(config)) { "The sign-in service returned unexpected endpoints." }
            val request = AuthorizationRequest.Builder(config, AuthConfig.CLIENT, ResponseTypeValues.CODE, Uri.parse(AuthConfig.REDIRECT))
                .setScope("openid profile offline_access")
                .setCodeVerifier(CodeVerifierUtil.generateRandomCodeVerifier())
                .apply {
                    if (provider != null) setAdditionalParameters(mapOf("kc_idp_hint" to provider))
                    if (provider != null || otherAccount) setPrompt("login")
                }.build()
            mutex.withLock {
                if (epoch != generation.get()) throw CancellationException()
                pending = request; pendingAt = System.currentTimeMillis(); persist()
            }
            service.getAuthorizationRequestIntent(request)
        } catch (failure: Exception) {
            withContext(NonCancellable) { mutex.withLock {
                if (epoch == generation.get()) mutable.value = mutable.value.copy(busy = false, error = "Could not open sign-in. Check your connection and try again.")
            } }
            if (failure is CancellationException) throw failure
            null
        }
    }

    override suspend fun verifyStrongSignIn(requirement: StrongSignInRequirement, expectedAuthorization: AuthorizationLease, open: (Intent) -> Unit): AuthorizationLease {
        initialized.join()
        token(null, expectedAuthorization)
        var own: StrongAttempt? = null
        try {
            val (attempt, config) = mutex.withLock {
                requireAuthorization(expectedAuthorization)
                if (!mutable.value.signedIn || !mutable.value.termsAccepted) throw StrongSignInException()
                val subject = StrongSignInProof.subject(auth?.accessToken, AuthConfig.ISSUER) ?: throw StrongSignInException()
                val config = auth?.authorizationServiceConfiguration?.takeIf(::trusted) ?: throw StrongSignInException()
                val epoch = generation.incrementAndGet()
                abortStrong(); pending = null; pendingAt = 0
                val attempt = StrongAttempt(expectedAuthorization, subject, requirement, System.currentTimeMillis() / 1000, epoch)
                own = attempt; strong = attempt; persist(); publish()
                attempt to config
            }
            val request = AuthorizationRequest.Builder(config, AuthConfig.CLIENT, ResponseTypeValues.CODE, Uri.parse(AuthConfig.REDIRECT))
                .setScope("openid profile offline_access").setCodeVerifier(CodeVerifierUtil.generateRandomCodeVerifier())
                .setPrompt("login").setAdditionalParameters(mapOf("acr_values" to requirement.requestedACR, "max_age" to requirement.maxAge.toString())).build()
            mutex.withLock {
                requireAuthorization(expectedAuthorization)
                if (strong !== attempt || generation.get() != attempt.epoch || attempt.stage.get() != 0) throw AuthorizationRetired()
                pending = request; pendingAt = System.currentTimeMillis(); persist()
            }
            withContext(Dispatchers.Main.immediate) {
                if (strong !== attempt || generation.get() != attempt.epoch || !expectedAuthorization.isValid || attempt.stage.get() != 0) throw AuthorizationRetired()
                open(service.getAuthorizationRequestIntent(request))
            }
            return withTimeout(600_000) { attempt.result.await() }
        } finally {
            own?.let { attempt ->
                attempt.stage.compareAndSet(0, 2)
                withContext(NonCancellable) { mutex.withLock {
                    if (strong === attempt) {
                        generation.incrementAndGet(); strong = null; pending = null; pendingAt = 0
                        attempt.result.cancel(); persist(); publish()
                    }
                } }
            }
        }
    }

    private fun abortStrong() {
        strong?.let { it.stage.compareAndSet(0, 2); it.result.cancel() }
        strong = null
    }

    override suspend fun complete(result: Intent?) {
        initialized.join()
        val response = result?.let(AuthorizationResponse::fromIntent)
        val failure = result?.let(AuthorizationException::fromIntent)
        if (response == null) {
            cancel()
            if (failure != null && failure.code != AuthorizationException.GeneralErrors.USER_CANCELED_AUTH_FLOW.code)
                mutable.value = mutable.value.copy(error = "Sign-in did not finish. Please try again.")
            return
        }
        val epoch = generation.get()
        var proof: StrongAttempt? = null
        var nonce: String? = null
        try {
            val candidate = mutex.withLock {
                val expected = pending ?: return
                // A delayed callback is not allowed to cancel the request that replaced it.
                if (!(response.state == expected.state && response.request.state == expected.state
                    && response.request.clientId == AuthConfig.CLIENT && response.request.redirectUri.toString() == AuthConfig.REDIRECT
                    && response.request.codeVerifier == expected.codeVerifier && response.request.nonce == expected.nonce
                    && trusted(response.request.configuration))) return
                require(System.currentTimeMillis() - pendingAt in 0..600000) { "Sign-in expired. Please try again." }
                proof = strong; nonce = expected.nonce
                proof?.let { requireAuthorization(it.authorization); if (it.epoch != epoch || it.stage.get() != 0) throw AuthorizationRetired() }
                pending = null; pendingAt = 0; persist()
                AuthState(response, failure)
            }
            val tokens = exchange(response.createTokenExchangeRequest())
            candidate.update(tokens, null)
            val verified = proof
            if (verified != null) {
                if (strong !== verified || generation.get() != epoch || !verified.authorization.isValid || verified.stage.get() != 0) throw AuthorizationRetired()
                StrongSignInProof.validate(tokens.idToken, tokens.accessToken ?: throw StrongSignInException(), verified.subject, nonce ?: throw StrongSignInException(),
                    verified.requirement, AuthConfig.ISSUER, AuthConfig.CLIENT, verified.began)
            }
            mutex.withLock {
                if (generation.get() != epoch) throw CancellationException()
                require(candidate.isAuthorized && candidate.accessToken != null) { "Sign-in returned no session." }
                if (verified != null) {
                    requireAuthorization(verified.authorization)
                    if (strong !== verified || !verified.stage.compareAndSet(0, 1)) throw AuthorizationRetired()
                }
                withContext(NonCancellable) {
                    val retainedTerms = if (verified != null) acceptedTerms else null
                    val previous = acceptedAuthorization
                    previous?.retire()
                    acceptedAuthorization = null; auth = null; acceptedTerms = null; publish(loading = verified != null)
                    previous?.awaitQuiescence()
                    try {
                        if (generation.get() != epoch) throw CancellationException()
                        auth = candidate; acceptedTerms = retainedTerms; persist()
                        if (generation.get() != epoch) throw CancellationException()
                        acceptedAuthorization = AuthorizationLease()
                        if (strong === verified) strong = null
                        publish()
                        verified?.let { it.stage.set(3); it.result.complete(acceptedAuthorization!!) }
                    } catch (failure: Exception) {
                        auth = null; acceptedTerms = null; publish()
                        runCatching { persist() }
                        throw failure
                    }
                }
            }
        } catch (failure: Exception) {
            proof?.result?.completeExceptionally(if (failure is CancellationException || failure is StrongSignInException) failure else StrongSignInException())
            if (failure is CancellationException) throw failure
            mutex.withLock {
                if (generation.get() == epoch) {
                    if (strong === proof) strong = null
                    pending = null; pendingAt = 0; persist(); publish()
                    mutable.value = mutable.value.copy(busy = false, error = if (failure is StrongSignInException) failure.message else "Sign-in could not be completed. Please try again.")
                }
            }
        }
    }

    override suspend fun token(rejected: String?): String {
        initialized.join()
        return token(rejected, callerAuthorization())
    }
    override suspend fun token(rejected: String?, expectedAuthorization: AuthorizationLease?): String {
        initialized.join()
        return mutex.withLock {
            requireAuthorization(expectedAuthorization)
            val current = auth ?: throw SessionExpired()
            val epoch = generation.get()
            val access = current.accessToken
            val expires = current.accessTokenExpirationTime ?: 0
            if (access != null && access != rejected && expires > System.currentTimeMillis() + 60000) return@withLock access
            if (current.refreshToken == null) { clearLocked(); throw SessionExpired() }
            try {
                val tokens = exchange(current.createTokenRefreshRequest())
                if (epoch != generation.get()) throw CancellationException()
                requireAuthorization(expectedAuthorization)
                current.update(tokens, null); persist(); publish()
                current.accessToken ?: throw SessionExpired()
            } catch (failure: AuthorizationException) {
                if (failure.error == "invalid_grant") { clearLocked(); throw SessionExpired() }
                throw IOException("Could not renew the session. Try again when connected.")
            }
        }
    }
    override suspend fun invalidate(rejected: String) {
        initialized.join()
        invalidate(rejected, callerAuthorization())
    }
    override suspend fun invalidate(rejected: String, expectedAuthorization: AuthorizationLease?) {
        initialized.join()
        mutex.withLock {
            requireAuthorization(expectedAuthorization)
            if (auth?.accessToken == rejected) { generation.incrementAndGet(); clearLocked() }
        }
    }
    override suspend fun cancel() {
        generation.incrementAndGet()
        mutex.withLock { abortStrong(); pending = null; pendingAt = 0; persist(); publish() }
    }
    override suspend fun acceptTerms() {
        initialized.join()
        mutex.withLock {
            if (auth?.isAuthorized != true) return
            val previous = acceptedTerms
            mutable.value = mutable.value.copy(busy = true, error = null)
            try {
                acceptedTerms = AppPolicy.TERMS_VERSION
                persist()
                publish()
            } catch (cancelled: CancellationException) {
                acceptedTerms = previous
                publish()
                throw cancelled
            } catch (_: Exception) {
                acceptedTerms = previous
                mutable.value = mutable.value.copy(busy = false, error = "Could not save your acceptance. Please try again.")
            }
        }
    }
    override suspend fun signOut() {
        initialized.join()
        signOut(callerAuthorization())
    }
    override suspend fun signOut(expectedAuthorization: AuthorizationLease?) {
        initialized.join()
        val refresh = mutex.withLock {
            if (acceptedAuthorization !== expectedAuthorization) throw AuthorizationRetired()
            generation.incrementAndGet()
            val token = auth?.refreshToken; clearLocked(); token
        }
        if (refresh != null) withContext(Dispatchers.IO) {
            val http = OkHttpClient.Builder().followRedirects(false).retryOnConnectionFailure(false).callTimeout(15, TimeUnit.SECONDS).build()
            val body = FormBody.Builder().add("client_id", AuthConfig.CLIENT).add("refresh_token", refresh).build()
            try { http.newCall(Request.Builder().url(AuthConfig.LOGOUT).post(body).build()).await().close() }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (_: IOException) { /* Local sign-out must succeed even while disconnected. */ }
        }
    }

    private suspend fun persist() = withContext(Dispatchers.IO) {
        val json = JSONObject().apply {
            auth?.let {
                put("auth", it.jsonSerializeString())
                acceptedTerms?.let { version -> put("accepted_terms", version) }
            }
            pending?.let { put("pending", it.jsonSerializeString()); put("pending_at", pendingAt); if (strong != null) put("pending_strong", true) }
        }
        store.write(if (auth == null && pending == null) null else json.toString())
    }
    private suspend fun clearLocked() {
        withContext(NonCancellable) {
            val previous = acceptedAuthorization
            previous?.retire()
            abortStrong(); acceptedAuthorization = null; auth = null; acceptedTerms = null; pending = null; pendingAt = 0
            publish()
            previous?.awaitQuiescence()
            persist()
        }
    }
    private fun requireAuthorization(expected: AuthorizationLease?) {
        if (acceptedAuthorization !== expected) throw AuthorizationRetired()
        expected?.requireActive() ?: throw SessionExpired()
    }
    private suspend fun callerAuthorization(): AuthorizationLease? {
        val accepted = acceptedAuthorization
        currentCoroutineContext()[AuthorizationContext]?.let { context ->
            val caller = context.lease ?: throw AuthorizationRetired()
            caller.requireActive()
            if (accepted == null || !caller.sharesAuthorization(accepted)) throw AuthorizationRetired()
        }
        return accepted
    }
    private fun publish(loading: Boolean = false) {
        mutable.value = SessionState(loading = loading, signedIn = auth?.isAuthorized == true, busy = strong != null, username = username(auth?.accessToken),
            termsAccepted = auth?.isAuthorized == true && acceptedTerms == AppPolicy.TERMS_VERSION, authorizationId = acceptedAuthorization?.id,
            subject = StrongSignInProof.subject(auth?.accessToken, AuthConfig.ISSUER))
    }
    private fun trusted(config: AuthorizationServiceConfiguration) =
        config.authorizationEndpoint.toString() == AuthConfig.AUTH && config.tokenEndpoint.toString() == AuthConfig.TOKEN
    private suspend fun discover(): AuthorizationServiceConfiguration = suspendCancellableCoroutine { continuation ->
        AuthorizationServiceConfiguration.fetchFromIssuer(Uri.parse(AuthConfig.ISSUER)) { config, error ->
            if (continuation.isActive) {
                if (config != null) continuation.resume(config) else continuation.resumeWithException(error ?: IOException("Could not reach sign-in."))
            }
        }
    }
    private suspend fun exchange(request: TokenRequest): TokenResponse = suspendCancellableCoroutine { continuation ->
        service.performTokenRequest(request) { response, error ->
            if (continuation.isActive) {
                if (response?.accessToken != null) continuation.resume(response) else continuation.resumeWithException(error ?: IOException("No token was returned."))
            }
        }
    }
    private fun username(token: String?): String? = runCatching {
        val encoded = token?.split('.')?.getOrNull(1) ?: return null
        JSONObject(String(Base64.decode(encoded, Base64.URL_SAFE or Base64.NO_WRAP or Base64.NO_PADDING)))
            .optString("preferred_username").takeIf { it.isNotEmpty() }
    }.getOrNull()
}
