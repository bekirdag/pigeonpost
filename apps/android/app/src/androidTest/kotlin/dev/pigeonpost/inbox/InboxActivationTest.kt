package dev.pigeonpost.inbox

import android.app.Application
import android.content.Intent
import androidx.lifecycle.ViewModelStore
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.pigeonpost.core.AuthorizationLease
import dev.pigeonpost.inbox.auth.SessionState
import dev.pigeonpost.inbox.auth.UserSession
import dev.pigeonpost.inbox.calls.CallsOverride
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.first
import org.json.JSONTokener
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import kotlin.coroutines.resume

/** Same canonical inbox and immediate fixture responses through actual ViewModel/WebView binding. */
@RunWith(AndroidJUnit4::class)
class InboxActivationTest {
    private class Session : UserSession {
        var grant = AuthorizationLease()
        override val authorizationLease get() = grant
        override val state = MutableStateFlow(SessionState(loading = false, signedIn = true, termsAccepted = true, authorizationId = grant.id, subject = "member"))
        override suspend fun token(rejected: String?) = "fixture-" + grant.id
        override suspend fun begin(provider: String?, otherAccount: Boolean): Intent? = null
        override suspend fun complete(result: Intent?) {}
        override suspend fun cancel() {}
        override suspend fun acceptTerms() {}
        override suspend fun signOut() { grant.retire(); state.value = SessionState(loading = false) }
        fun replace() { grant.retire(); grant = AuthorizationLease(); state.value = state.value.copy(authorizationId = grant.id) }
    }
    private suspend fun js(model: InboxViewModel, expression: String): Any? = withContext(Dispatchers.Main) {
        suspendCancellableCoroutine { continuation ->
            model.calls!!.webView.evaluateJavascript("JSON.stringify($expression)") { text ->
                val decoded = runCatching { JSONTokener(text).nextValue() }.getOrNull()
                val value = if (decoded is String) runCatching { JSONTokener(decoded).nextValue() }.getOrNull() else decoded
                if (continuation.isActive) continuation.resume(value)
            }
        }
    }
    @Test fun sameInboxLoginReplacementRebindsCalendarAndTheCallPage() = runBlocking<Unit> {
        ScheduledCallHostTest.Page().use { page ->
            val app = ApplicationProvider.getApplicationContext<Application>()
            app.getSharedPreferences("fixture-settings", 0).edit().clear().commit()
            val session = Session()
            val fixture = Development.graph(app, Intent().putExtra("pigeonpost.fixtures", "calendar"))!!
            val owners = ViewModelStore()
            val model = withContext(Dispatchers.Main) { InboxViewModel(app, fixture.copy(session = session), CallsOverride(page.url, null, null, null, null)).also { owners.put("activation", it) } }
            try {
                val calendar = model.calendar!!
                val original = withTimeout(10_000) { calendar.state.first { it.enabled && it.mailbox == "/k/demomain" } }.contextId
                withTimeout(10_000) { while (js(model, "window.options && window.options.token") != "fixture-" + session.grant.id) delay(20) }
                val old = session.grant
                withContext(Dispatchers.Main) { session.replace() } // No signed-out state is published.
                val replacement = withTimeout(10_000) { calendar.state.first { it.enabled && it.mailbox == "/k/demomain" && it.contextId != original } }
                assertNotEquals(original, replacement.contextId); assertFalse(old.isValid)
                withTimeout(10_000) { while (js(model, "window.options && window.options.token") != "fixture-" + session.grant.id) delay(20) }
                assertEquals("/k/demomain", js(model, "window.options.mailbox.address"))
            } finally { withContext(Dispatchers.Main) { owners.clear() } }
        }
    }
    @Test fun anAuthorizationIdWithoutItsAcceptedGrantCannotLoadAnInbox() = runBlocking<Unit> {
        val app = ApplicationProvider.getApplicationContext<Application>()
        app.getSharedPreferences("fixture-settings", 0).edit().clear().commit()
        val session = Session().apply { state.value = state.value.copy(authorizationId = "unrelated-context") }
        val fixture = Development.graph(app, Intent().putExtra("pigeonpost.fixtures", "calendar"))!!
        val owners = ViewModelStore()
        val model = withContext(Dispatchers.Main) { InboxViewModel(app, fixture.copy(session = session)).also { owners.put("activation", it) } }
        try {
            withContext(Dispatchers.Main) { yield() }; delay(150)
            assertNull(model.inbox.state.value.acting); assertFalse(model.calendar!!.state.value.enabled)
            withContext(Dispatchers.Main) { session.state.value = session.state.value.copy(authorizationId = session.grant.id) }
            withTimeout(10_000) { model.calendar!!.state.first { it.enabled && it.mailbox == "/k/demomain" } }
        } finally { withContext(Dispatchers.Main) { owners.clear() } }
    }
}
