package dev.pigeonpost.inbox.auth

import android.content.Context
import android.content.Intent
import android.net.Uri
import android.util.Base64
import dev.pigeonpost.core.SessionExpired
import dev.pigeonpost.core.TokenProvider
import dev.pigeonpost.core.await
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.suspendCancellableCoroutine
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
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
data class SessionState(val loading: Boolean = true, val signedIn: Boolean = false, val busy: Boolean = false, val username: String? = null, val error: String? = null)
interface UserSession : TokenProvider {
    val state: StateFlow<SessionState>
    suspend fun begin(provider: String? = null, otherAccount: Boolean = false): Intent?
    suspend fun complete(result: Intent?)
    suspend fun cancel()
    suspend fun signOut()
}

class Session(context: Context) : UserSession {
    private val store = SecureStore(context)
    private val service = AuthorizationService(context)
    private val mutex = Mutex()
    private val generation = AtomicLong(0)
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
    private var auth: AuthState? = null
    private var pending: AuthorizationRequest? = null
    private var pendingAt = 0L
    private val mutable = MutableStateFlow(SessionState())
    override val state = mutable.asStateFlow()
    private val initialized = scope.launch {
        mutex.withLock {
            try {
                store.read()?.let { text ->
                    val saved = JSONObject(text)
                    saved.optString("auth").takeIf { it.isNotEmpty() }?.let { auth = AuthState.jsonDeserialize(it) }
                    saved.optString("pending").takeIf { it.isNotEmpty() }?.let { pending = AuthorizationRequest.jsonDeserialize(it) }
                    pendingAt = saved.optLong("pending_at")
                    auth?.authorizationServiceConfiguration?.let { require(trusted(it)) }
                }
                publish()
            } catch (_: Exception) {
                auth = null; pending = null; store.write(null)
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
            if (epoch == generation.get()) mutable.value = mutable.value.copy(busy = false, error = "Could not open sign-in. Check your connection and try again.")
            if (failure is CancellationException) throw failure
            null
        }
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
        try {
            val candidate = mutex.withLock {
                val expected = pending ?: throw IOException("No sign-in is waiting.")
                require(System.currentTimeMillis() - pendingAt in 0..600000) { "Sign-in expired. Please try again." }
                require(response.state == expected.state && response.request.state == expected.state
                    && response.request.clientId == AuthConfig.CLIENT && response.request.redirectUri.toString() == AuthConfig.REDIRECT
                    && response.request.codeVerifier == expected.codeVerifier && response.request.nonce == expected.nonce
                    && trusted(response.request.configuration)) { "Sign-in could not be verified." }
                pending = null; pendingAt = 0; persist()
                AuthState(response, failure)
            }
            val tokens = exchange(response.createTokenExchangeRequest())
            candidate.update(tokens, null)
            mutex.withLock {
                if (generation.get() != epoch) throw CancellationException()
                require(candidate.isAuthorized && candidate.accessToken != null) { "Sign-in returned no session." }
                auth = candidate; persist(); publish()
            }
        } catch (failure: Exception) {
            if (failure is CancellationException) throw failure
            if (generation.get() == epoch) {
                mutex.withLock { pending = null; persist() }
                mutable.value = mutable.value.copy(busy = false, error = "Sign-in could not be completed. Please try again.")
            }
        }
    }

    override suspend fun token(rejected: String?): String {
        initialized.join()
        return mutex.withLock {
            val current = auth ?: throw SessionExpired()
            val epoch = generation.get()
            val access = current.accessToken
            val expires = current.accessTokenExpirationTime ?: 0
            if (access != null && access != rejected && expires > System.currentTimeMillis() + 60000) return@withLock access
            if (current.refreshToken == null) { clearLocked(); throw SessionExpired() }
            try {
                val tokens = exchange(current.createTokenRefreshRequest())
                if (epoch != generation.get()) throw CancellationException()
                current.update(tokens, null); persist(); publish()
                current.accessToken ?: throw SessionExpired()
            } catch (failure: AuthorizationException) {
                if (failure.error == "invalid_grant") { clearLocked(); throw SessionExpired() }
                throw IOException("Could not renew the session. Try again when connected.")
            }
        }
    }
    override suspend fun invalidate(rejected: String) {
        mutex.withLock { if (auth?.accessToken == rejected) { generation.incrementAndGet(); clearLocked() } }
    }
    override suspend fun cancel() {
        generation.incrementAndGet()
        mutex.withLock { pending = null; pendingAt = 0; persist(); publish() }
    }
    override suspend fun signOut() {
        generation.incrementAndGet()
        initialized.join()
        val refresh = mutex.withLock { val token = auth?.refreshToken; clearLocked(); token }
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
            auth?.let { put("auth", it.jsonSerializeString()) }
            pending?.let { put("pending", it.jsonSerializeString()); put("pending_at", pendingAt) }
        }
        store.write(if (auth == null && pending == null) null else json.toString())
    }
    private suspend fun clearLocked() {
        auth = null; pending = null; pendingAt = 0; persist(); publish()
    }
    private fun publish() {
        mutable.value = SessionState(loading = false, signedIn = auth?.isAuthorized == true, username = username(auth?.accessToken))
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
