package dev.pigeonpost.inbox

import android.app.Application
import android.content.Intent

/** Release builds have no fixture session or postbox implementation. */
object Development {
    @Suppress("UNUSED_PARAMETER")
    fun graph(application: Application, intent: Intent): AppGraph? = null
}
