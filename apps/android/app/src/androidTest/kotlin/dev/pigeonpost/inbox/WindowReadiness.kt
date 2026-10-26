package dev.pigeonpost.inbox

import android.accessibilityservice.AccessibilityServiceInfo
import android.app.KeyguardManager
import android.graphics.Rect
import android.hardware.display.DisplayManager
import android.os.ParcelFileDescriptor
import android.os.PowerManager
import androidx.compose.ui.test.ComposeTimeoutException
import androidx.compose.ui.test.junit4.ComposeTestRule
import androidx.test.platform.app.InstrumentationRegistry

/** Keep the real input-focus assertion and report only system state and window owner packages. */
internal fun awaitAndroidWindowFocus(ui: ComposeTestRule, focused: () -> Boolean) {
    try { ui.waitUntil(10_000, focused) }
    catch (failure: ComposeTimeoutException) { throw AssertionError("Android input window did not become ready. ${windowInputStatus()}", failure) }
}

/** No screen text, titles, URLs, intents, tokens or account data enter the report. */
internal fun windowInputStatus(): String = runCatching {
    val instrumentation = InstrumentationRegistry.getInstrumentation()
    val context = instrumentation.targetContext
    val power = context.getSystemService(PowerManager::class.java)
    val keyguard = context.getSystemService(KeyguardManager::class.java)
    val displays = context.getSystemService(DisplayManager::class.java).displays.joinToString { "${it.displayId}:${it.state}" }
    val automation = instrumentation.uiAutomation
    val info = automation.serviceInfo
    val previous = info.flags
    val windows = try {
        info.flags = previous or AccessibilityServiceInfo.FLAG_RETRIEVE_INTERACTIVE_WINDOWS
        automation.serviceInfo = info
        automation.windows.joinToString { window ->
            val bounds = Rect().also(window::getBoundsInScreen)
            val owner = window.root?.packageName?.toString()?.takeIf { it.matches(Regex("[A-Za-z0-9_.]{1,160}")) } ?: "unknown"
            "type=${window.type},layer=${window.layer},focused=${window.isFocused},active=${window.isActive},owner=$owner,bounds=${bounds.toShortString()}"
        }
    } finally { info.flags = previous; automation.serviceInfo = info }
    val managerOwners = runCatching {
        ParcelFileDescriptor.AutoCloseInputStream(automation.executeShellCommand("dumpsys window windows")).bufferedReader().use { input ->
            input.lineSequence().filter { "mCurrentFocus=" in it || "mFocusedApp=" in it }.map { line ->
                val component = Regex("[A-Za-z][A-Za-z0-9_]*(?:\\.[A-Za-z][A-Za-z0-9_]*)+/[A-Za-z0-9_.$]+")
                (if ("mCurrentFocus=" in line) "focus" else "app") + "=" + (component.find(line)?.value ?: "none")
            }.joinToString()
        }
    }.getOrDefault("unavailable")
    "interactive=${power.isInteractive},keyguard=${keyguard.isKeyguardLocked},deviceLocked=${keyguard.isDeviceLocked},displays=[$displays],windows=[$windows],manager=[$managerOwners],runner=[${PigeonpostTestRunner.startupReadiness}]"
}.getOrElse { "Window diagnostics unavailable (${it.javaClass.simpleName})." }
