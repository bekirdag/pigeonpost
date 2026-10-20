package dev.pigeonpost.inbox

import android.app.Application
import android.content.Intent
import dev.pigeonpost.inbox.calls.CallsOverride

/** Release builds have no fixture session, postbox implementation, call harness or appearance pin. */
object Development {
    @Suppress("UNUSED_PARAMETER")
    fun graph(application: Application, intent: Intent): AppGraph? = null
    @Suppress("UNUSED_PARAMETER")
    fun calls(intent: Intent): CallsOverride? = null
    @Suppress("UNUSED_PARAMETER")
    fun appearance(intent: Intent): String? = null
    fun started() {}
}
