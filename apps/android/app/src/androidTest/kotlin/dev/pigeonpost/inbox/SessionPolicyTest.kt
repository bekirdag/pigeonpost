package dev.pigeonpost.inbox

import android.content.Context
import android.content.ContextWrapper
import android.net.Uri
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.pigeonpost.inbox.auth.AuthConfig
import dev.pigeonpost.inbox.auth.SecureStore
import dev.pigeonpost.inbox.auth.Session
import dev.pigeonpost.core.AuthorizationLease
import dev.pigeonpost.core.AuthorizationRetired
import dev.pigeonpost.core.AuthorizationContext
import kotlinx.coroutines.withContext
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.withTimeout
import net.openid.appauth.AuthState
import net.openid.appauth.AuthorizationServiceConfiguration
import net.openid.appauth.TokenRequest
import net.openid.appauth.TokenResponse
import org.json.JSONObject
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.util.UUID

@RunWith(AndroidJUnit4::class)
class SessionPolicyTest {
    private val context get() = ApplicationProvider.getApplicationContext<Context>()
    private fun authState(): AuthState {
        val config = AuthorizationServiceConfiguration(Uri.parse(AuthConfig.AUTH), Uri.parse(AuthConfig.TOKEN))
        val request = TokenRequest.Builder(config, AuthConfig.CLIENT)
            .setGrantType("authorization_code").setAuthorizationCode("fixture-code")
            .setRedirectUri(Uri.parse(AuthConfig.REDIRECT)).build()
        return AuthState(config).apply {
            update(TokenResponse.Builder(request).setAccessToken("fixture-access-token").setAccessTokenExpirationTime(System.currentTimeMillis() + 3600000).setTokenType("Bearer").build(), null)
        }
    }
    private fun saved(version: String? = null) = JSONObject().apply {
        put("auth", authState().jsonSerializeString())
        version?.let { put("accepted_terms", it) }
    }.toString()
    private suspend fun ready(session: Session) = withTimeout(10000) { session.state.first { !it.loading } }

    @Test fun acceptanceSurvivesProcessRestoreButSignOutErasesIt() = runBlocking {
        val root = File(context.cacheDir, "policy-${UUID.randomUUID()}").apply { mkdirs() }
        val isolated = object : ContextWrapper(context) { override fun getNoBackupFilesDir() = root }
        try {
            val storage = SecureStore(isolated)
            storage.write(saved())
            val session = Session(isolated)
            assertTrue(ready(session).signedIn)
            assertFalse(session.state.value.termsAccepted)
            session.acceptTerms()
            assertTrue(session.state.value.termsAccepted)
            assertEquals(AppPolicy.TERMS_VERSION, JSONObject(storage.read()!!).getString("accepted_terms"))

            val restored = Session(isolated)
            assertTrue(ready(restored).termsAccepted)
            restored.signOut()
            assertFalse(restored.state.value.signedIn)
            assertFalse(restored.state.value.termsAccepted)
            assertNull(storage.read())

            // A subsequent signed-in session gets no consent from the previous account.
            storage.write(saved())
            assertFalse(ready(Session(isolated)).termsAccepted)
        } finally { root.deleteRecursively() }
    }

    @Test fun obsoleteTermsAndSignedOutSessionsCannotBypassConsent() = runBlocking {
        val root = File(context.cacheDir, "policy-${UUID.randomUUID()}").apply { mkdirs() }
        val isolated = object : ContextWrapper(context) { override fun getNoBackupFilesDir() = root }
        try {
            val storage = SecureStore(isolated)
            storage.write(saved("obsolete-policy"))
            val session = Session(isolated)
            assertTrue(ready(session).signedIn)
            assertFalse(session.state.value.termsAccepted)
            session.signOut()
            session.acceptTerms()
            assertFalse(session.state.value.termsAccepted)
            assertNull(storage.read())
        } finally { root.deleteRecursively() }
    }

    @Test fun staleLogoutAndInvalidationCannotTouchTheAcceptedGrant() = runBlocking {
        val root = File(context.cacheDir, "authorization-${UUID.randomUUID()}").apply { mkdirs() }
        val isolated = object : ContextWrapper(context) { override fun getNoBackupFilesDir() = root }
        try {
            val storage = SecureStore(isolated)
            storage.write(saved(AppPolicy.TERMS_VERSION))
            val session = Session(isolated)
            assertTrue(ready(session).termsAccepted)
            val accepted = session.authorizationLease!!
            assertEquals(accepted.id, session.state.value.authorizationId)
            val stale = AuthorizationLease().apply { retire() }
            suspend fun refused(action: suspend () -> Unit) {
                try { action(); fail("Retired authorization was accepted") }
                catch (_: AuthorizationRetired) { }
            }
            refused { session.token(null, stale) }
            refused { session.invalidate("fixture-access-token", stale) }
            refused { session.signOut(stale) }
            withContext(AuthorizationContext(stale)) {
                refused { session.token() }
                refused { session.invalidate("fixture-access-token") }
                refused { session.signOut() }
            }
            withContext(AuthorizationContext(null)) { refused { session.token() } }
            withContext(AuthorizationContext(AuthorizationLease())) { refused { session.token() } }
            withContext(AuthorizationContext(accepted.child())) { assertEquals("fixture-access-token", session.token()) }
            assertSame(accepted, session.authorizationLease)
            assertTrue(accepted.isValid)
            assertEquals("fixture-access-token", session.token(null, accepted))
            session.acceptTerms(); session.cancel()
            assertSame(accepted, session.authorizationLease)
            assertNotNull(storage.read())

            session.signOut(accepted)
            assertFalse(accepted.isValid)
            assertNull(session.authorizationLease)
            assertNull(session.state.value.authorizationId)
            assertNull(storage.read())
            refused { session.token(null, accepted) }
        } finally { root.deleteRecursively() }
    }

    @Test fun processRestoreCreatesAnIndependentAuthorizationIdentity() = runBlocking {
        val root = File(context.cacheDir, "restore-grant-${UUID.randomUUID()}").apply { mkdirs() }
        val isolated = object : ContextWrapper(context) { override fun getNoBackupFilesDir() = root }
        try {
            SecureStore(isolated).write(saved(AppPolicy.TERMS_VERSION))
            val first = Session(isolated); ready(first)
            val restored = Session(isolated); ready(restored)
            assertNotEquals(first.state.value.authorizationId, restored.state.value.authorizationId)
            assertFalse(first.authorizationLease!!.sharesAuthorization(restored.authorizationLease!!))
            restored.signOut()
        } finally { root.deleteRecursively() }
    }
}
