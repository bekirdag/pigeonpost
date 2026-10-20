package dev.pigeonpost.inbox

import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import dev.pigeonpost.inbox.calls.IncomingCalls
import dev.pigeonpost.inbox.push.displayPush

/**
 * Debug builds only: an FCM-shaped data message delivered over adb, for testing incoming calls on an
 * emulator without Firebase. Every string extra becomes a data field, and it goes through the same
 * handler as a real push:
 *
 * `adb shell am broadcast -n dev.pigeonpost.inbox.debug/dev.pigeonpost.inbox.DebugPushReceiver
 *   -a dev.pigeonpost.inbox.debug.PUSH --es type call_ring --es room … --es ring_token … …`
 */
class DebugPushReceiver : BroadcastReceiver() {
    override fun onReceive(context: Context, intent: Intent) {
        if (intent.action != "dev.pigeonpost.inbox.debug.PUSH") return
        val extras = intent.extras ?: return
        val data = extras.keySet().mapNotNull { key -> extras.getString(key)?.let { key to it } }.toMap()
        if (!IncomingCalls.push(context, data)) displayPush(context, data)
    }
}
