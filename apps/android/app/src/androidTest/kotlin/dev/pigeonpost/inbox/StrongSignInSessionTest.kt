package dev.pigeonpost.inbox

import android.content.ActivityNotFoundException
import android.content.Context
import android.content.ContextWrapper
import android.content.Intent
import android.net.Uri
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.pigeonpost.core.*
import dev.pigeonpost.inbox.auth.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.first
import net.openid.appauth.*
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.io.ByteArrayInputStream
import java.io.ByteArrayOutputStream
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.util.Base64
import java.util.UUID
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit
import java.util.concurrent.atomic.AtomicInteger

/** Real Session, encrypted storage and AppAuth parser, with an in-memory token connection only. */
@RunWith(AndroidJUnit4::class)
class StrongSignInSessionTest {
    private val requirement = StrongSignInRequirement.create(listOf("2"), 600)!!
    private data class Candidate(val subject: String = "member", val acr: String = "2", val age: Long = 0, val nonce: String? = null)
    private fun jwt(claims: JSONObject): String {
        fun encoded(text: String) = Base64.getUrlEncoder().withoutPadding().encodeToString(text.toByteArray())
        return encoded("""{"alg":"RS256"}""") + "." + encoded(claims.toString()) + ".c2ln"
    }
    private fun response(candidate: Candidate, nonce: String): JSONObject {
        val now = System.currentTimeMillis() / 1000
        val access = JSONObject().put("iss", AuthConfig.ISSUER).put("sub", candidate.subject).put("azp", AuthConfig.CLIENT)
            .put("iat", now).put("exp", now + 3600).put("auth_time", now - candidate.age).put("acr", candidate.acr)
        val id = JSONObject(access.toString()).put("aud", AuthConfig.CLIENT).put("nonce", candidate.nonce ?: nonce)
        return JSONObject().put("token_type", "Bearer").put("access_token", jwt(access)).put("id_token", jwt(id)).put("expires_in", 3600)
            .put("scope", "openid profile offline_access")
    }
    private inner class Backend {
        @Volatile var request: AuthorizationRequest? = null
        @Volatile var candidate = Candidate()
        @Volatile var gate: CountDownLatch? = null
        val entered = CompletableDeferred<Unit>()
        val calls = AtomicInteger()
        fun connection(uri: Uri): HttpURLConnection {
            check(uri.toString() == AuthConfig.TOKEN) { "Unexpected fixture endpoint" }
            val expected = request ?: error("No fixture authorization request")
            val bytes = response(candidate, expected.nonce!!).toString().toByteArray()
            val waiting = gate
            calls.incrementAndGet()
            return object : HttpURLConnection(URL(uri.toString())) {
                override fun connect() {}
                override fun disconnect() {}
                override fun usingProxy() = false
                override fun getResponseCode() = 200
                override fun getOutputStream() = ByteArrayOutputStream()
                override fun getHeaderField(name: String?) = if (name.equals("Content-Type", true)) "application/json" else null
                override fun getInputStream(): ByteArrayInputStream {
                    entered.complete(Unit)
                    check(waiting?.await(8, TimeUnit.SECONDS) != false) { "Fixture token response timed out" }
                    return ByteArrayInputStream(bytes)
                }
            }
        }
    }
    private inner class Fixture {
        val root = File(ApplicationProvider.getApplicationContext<Context>().cacheDir, "strong-session-${UUID.randomUUID()}").apply { mkdirs() }
        val context = object : ContextWrapper(ApplicationProvider.getApplicationContext<Context>()) { override fun getNoBackupFilesDir() = root }
        val storage = SecureStore(context)
        val backend = Backend()
        val tasks = CoroutineScope(SupervisorJob() + Dispatchers.Default)
        val services = mutableListOf<AuthorizationService>()
        lateinit var session: Session
        private fun service() = AuthorizationService(context, AppAuthConfiguration.Builder().setConnectionBuilder(backend::connection).build()).also(services::add)
        suspend fun initialize() {
            val config = AuthorizationServiceConfiguration(Uri.parse(AuthConfig.AUTH), Uri.parse(AuthConfig.TOKEN))
            val request = TokenRequest.Builder(config, AuthConfig.CLIENT).setGrantType("authorization_code").setAuthorizationCode("fixture-code")
                .setRedirectUri(Uri.parse(AuthConfig.REDIRECT)).build()
            val value = response(Candidate(acr = "1", age = 1800), "initial-nonce")
            val auth = AuthState(config).apply { update(TokenResponse.Builder(request).setAccessToken(value.getString("access_token"))
                .setIdToken(value.getString("id_token")).setAccessTokenExpirationTime(System.currentTimeMillis() + 3_600_000).setTokenType("Bearer").build(), null) }
            storage.write(JSONObject().put("auth", auth.jsonSerializeString()).put("accepted_terms", AppPolicy.TERMS_VERSION).toString())
            session = Session(context, service()); ready(session)
            assertEquals("member", session.state.value.subject)
        }
        suspend fun restored(): Session = Session(context, service()).also { ready(it) }
        suspend fun start(): Pair<Deferred<Result<AuthorizationLease>>, AuthorizationRequest> {
            val opened = CompletableDeferred<Unit>()
            val grant = session.authorizationLease!!
            val result = tasks.async { runCatching { session.verifyStrongSignIn(requirement, grant) { opened.complete(Unit) } } }
            withTimeout(10_000) { opened.await() }
            val pending = AuthorizationRequest.jsonDeserialize(JSONObject(storage.read()!!).getString("pending"))
            backend.request = pending
            return result to pending
        }
        suspend fun complete(request: AuthorizationRequest) = session.complete(AuthorizationResponse.Builder(request).setState(request.state).setAuthorizationCode("fixture-code").build().toIntent())
        suspend fun close() = withContext(NonCancellable) {
            backend.gate?.countDown()
            if (::session.isInitialized) session.cancel()
            tasks.coroutineContext.job.cancelAndJoin()
            withContext(Dispatchers.Main) { services.forEach { it.dispose() } }
            root.deleteRecursively()
        }
    }
    private suspend fun ready(session: Session) = withTimeout(10_000) { session.state.first { !it.loading } }
    private suspend fun world(work: suspend Fixture.() -> Unit) {
        val fixture = Fixture()
        try { withTimeout(20_000) { fixture.initialize(); fixture.work() } }
        finally { fixture.close() }
    }
    @Test fun aFreshSamePersonInteractionRotatesTheGrantAndPreservesTheirTerms() = runBlocking { world {
        val old = session.authorizationLease!!
        val (result, request) = start()
        assertEquals("login", request.prompt); assertEquals("2", request.additionalParameters["acr_values"]); assertEquals("600", request.additionalParameters["max_age"])
        assertEquals("S256", request.codeVerifierChallengeMethod); assertFalse(request.nonce.isNullOrBlank()); assertEquals(0, backend.calls.get())
        complete(request)
        val accepted = result.await().getOrThrow()
        assertNotSame(old, accepted); assertFalse(old.isValid); assertSame(accepted, session.authorizationLease); assertTrue(accepted.isValid)
        assertTrue(session.state.value.termsAccepted); assertEquals("member", session.state.value.subject)
        assertFalse(JSONObject(storage.read()!!).has("pending")); assertFalse(session.state.value.busy)
        val restored = restored(); assertTrue(restored.state.value.termsAccepted); assertEquals("member", restored.state.value.subject)
    } }
    @Test fun anotherPersonsCandidateCannotReplaceTheAcceptedSession() = runBlocking { world {
        val old = session.authorizationLease!!; backend.candidate = Candidate(subject = "another-person")
        val (result, request) = start(); complete(request)
        val failure = result.await().exceptionOrNull()
        assertEquals(StrongSignInProblem.DIFFERENT_ACCOUNT, (failure as StrongSignInException).problem)
        assertSame(old, session.authorizationLease); assertTrue(old.isValid); assertEquals("member", session.state.value.subject); assertTrue(session.state.value.termsAccepted)
    } }
    @Test fun weakStaleAndNonceMismatchedCandidatesCannotReplaceTheAcceptedSession() = runBlocking { world {
        val old = session.authorizationLease!!
        for (candidate in listOf(Candidate(acr = "1"), Candidate(age = 631), Candidate(nonce = "wrong-nonce"))) {
            backend.candidate = candidate
            val (result, request) = start(); complete(request)
            val failure = result.await().exceptionOrNull()
            assertTrue(failure is StrongSignInException); assertFalse(failure.toString().contains("wrong-nonce"))
            assertSame(old, session.authorizationLease); assertTrue(old.isValid); assertTrue(session.state.value.termsAccepted)
        }
    } }
    @Test fun cancellationPreservesTheSessionAndOnlyFinishesItsOwnWaiter() = runBlocking { world {
        val old = session.authorizationLease!!; val (result, _) = start()
        session.cancel()
        assertTrue(result.await().exceptionOrNull() is CancellationException)
        assertSame(old, session.authorizationLease); assertTrue(old.isValid); assertTrue(session.state.value.termsAccepted)
        assertFalse(session.state.value.busy); assertFalse(JSONObject(storage.read()!!).has("pending")); assertEquals(0, backend.calls.get())
    } }
    @Test fun restoringAPendingStrongFlowNeverReinterpretsItAsOrdinarySignIn() = runBlocking { world {
        val (_, request) = start()
        assertTrue(JSONObject(storage.read()!!).getBoolean("pending_strong"))
        val restored = restored(); val original = restored.authorizationLease!!
        restored.complete(AuthorizationResponse.Builder(request).setState(request.state).setAuthorizationCode("fixture-code").build().toIntent())
        assertSame(original, restored.authorizationLease); assertTrue(restored.state.value.termsAccepted); assertFalse(restored.state.value.busy); assertEquals(0, backend.calls.get())
    } }
    @Test fun aStaleGrantCannotEvenOpenTheVerificationWindow() = runBlocking { world {
        var opened = false
        val failure = runCatching { session.verifyStrongSignIn(requirement, AuthorizationLease()) { opened = true } }.exceptionOrNull()
        assertTrue(failure is AuthorizationRetired); assertFalse(opened); assertEquals(0, backend.calls.get())
    } }
    @Test fun aLateCallbackCannotCancelTheRequestThatReplacedIt() = runBlocking { world {
        val (first, oldRequest) = start(); session.cancel(); assertTrue(first.await().isFailure)
        val (next, request) = start()
        complete(oldRequest)
        assertTrue(session.state.value.busy); assertFalse(next.isCompleted); assertEquals(0, backend.calls.get())
        assertEquals(request.state, AuthorizationRequest.jsonDeserialize(JSONObject(storage.read()!!).getString("pending")).state)
        complete(request); assertSame(next.await().getOrThrow(), session.authorizationLease)
    } }
    @Test fun aDelayedOldExchangeCannotAdoptOrClearAReplacementRequest() = runBlocking { world {
        val oldGrant = session.authorizationLease!!
        val (first, oldRequest) = start()
        val release = CountDownLatch(1); backend.gate = release
        val completion = tasks.async { runCatching { complete(oldRequest) } }
        withTimeout(10_000) { backend.entered.await() }
        session.cancel(); assertTrue(first.await().isFailure)
        val (next, request) = start(); backend.gate = null; release.countDown()
        assertTrue(completion.await().exceptionOrNull() is CancellationException)
        assertSame(oldGrant, session.authorizationLease); assertTrue(oldGrant.isValid)
        assertEquals(request.state, AuthorizationRequest.jsonDeserialize(JSONObject(storage.read()!!).getString("pending")).state)
        complete(request); assertSame(next.await().getOrThrow(), session.authorizationLease); assertEquals(2, backend.calls.get())
    } }
    @Test fun aFailedBrowserLaunchCleansThePendingFlowAndKeepsTheSession() = runBlocking { world {
        val old = session.authorizationLease!!
        val failure = runCatching { session.verifyStrongSignIn(requirement, old) { throw ActivityNotFoundException("fixture") } }.exceptionOrNull()
        assertTrue(failure is ActivityNotFoundException); assertSame(old, session.authorizationLease); assertTrue(old.isValid)
        assertFalse(session.state.value.busy); assertFalse(JSONObject(storage.read()!!).has("pending"))
    } }
    @Test fun anOrdinaryPendingRestoreDoesNotLockTheSignInControls() = runBlocking { world {
        val (_, request) = start()
        val saved = JSONObject(storage.read()!!).apply { remove("pending_strong") }; storage.write(saved.toString())
        val restored = restored()
        assertTrue(restored.state.value.signedIn); assertFalse(restored.state.value.busy)
        backend.request = request
        restored.complete(AuthorizationResponse.Builder(request).setState(request.state).setAuthorizationCode("fixture-code").build().toIntent())
        assertFalse(restored.state.value.termsAccepted) // Ordinary sign-in retains its existing consent behavior.
    } }
}
