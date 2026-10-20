package dev.pigeonpost.inbox

import android.app.Application
import android.content.Context
import android.content.Intent
import dev.pigeonpost.inbox.calls.CallsOverride

/** Release builds have no fixture session, postbox implementation, call harness, test pushes or appearance pin. */
object Development {
    @Suppress("UNUSED_PARAMETER")
    fun graph(application: Application, intent: Intent): AppGraph? = null
    @Suppress("UNUSED_PARAMETER")
    fun calls(intent: Intent): CallsOverride? = null
    @Suppress("UNUSED_PARAMETER")
    fun appearance(intent: Intent): String? = null
    fun started() {}
    @Suppress("UNUSED_PARAMETER")
    fun rememberHarness(context: Context, intent: Intent) {}
    @Suppress("UNUSED_PARAMETER")
    fun harnessExtras(context: Context, intent: Intent): Intent = intent
    @Suppress("UNUSED_PARAMETER")
    fun callIdentity(context: Context): String? = null
    @Suppress("UNUSED_PARAMETER")
    fun callsOrigin(context: Context): String? = null
}
