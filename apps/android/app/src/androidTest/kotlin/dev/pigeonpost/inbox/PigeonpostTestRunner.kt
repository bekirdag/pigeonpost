package dev.pigeonpost.inbox

import android.app.KeyguardManager
import android.os.Bundle
import android.os.ParcelFileDescriptor
import android.os.PowerManager
import android.os.SystemClock
import androidx.test.runner.AndroidJUnitRunner

/** Opt-in setup for disposable CI emulators; physical devices retain their normal lock policy. */
class PigeonpostTestRunner : AndroidJUnitRunner() {
    companion object { internal var startupReadiness = "default device setup" }
    private var prepareEmulator = false
    override fun onCreate(arguments: Bundle?) {
        prepareEmulator = arguments?.getString("pigeonpost_test_emulator") == "true"
        super.onCreate(arguments)
    }
    private fun shell(command: String): String = ParcelFileDescriptor.AutoCloseInputStream(uiAutomation.executeShellCommand(command)).bufferedReader().use { it.readText().trim() }
    override fun onStart() {
        if (prepareEmulator) {
            check(shell("getprop ro.kernel.qemu") == "1") { "Emulator test setup requires an actual emulator." }
            val power = targetContext.getSystemService(PowerManager::class.java)
            val keyguard = targetContext.getSystemService(KeyguardManager::class.java)
            startupReadiness = "beforeInteractive=${power.isInteractive},beforeKeyguard=${keyguard.isKeyguardLocked}"
            // API 34's first-boot Pixel Launcher can leave its own ANR dialog above every
            // test activity. Reset only that launcher on the opted-in disposable emulator,
            // before tests start. App/unknown error dialogs and focus assertions stay intact.
            check(shell("am force-stop com.google.android.apps.nexuslauncher").isBlank()) {
                "Unable to reset the disposable emulator's Pixel Launcher."
            }
            startupReadiness += ",launcherReset=true"
            if (!power.isInteractive) shell("input keyevent KEYCODE_WAKEUP")
            if (keyguard.isKeyguardLocked) shell("wm dismiss-keyguard")
            val deadline = SystemClock.uptimeMillis() + 5000
            while ((!power.isInteractive || keyguard.isKeyguardLocked) && SystemClock.uptimeMillis() < deadline) Thread.sleep(50)
            startupReadiness += ",afterInteractive=${power.isInteractive},afterKeyguard=${keyguard.isKeyguardLocked}"
            check(power.isInteractive && !keyguard.isKeyguardLocked) { "The disposable test emulator is not ready: $startupReadiness" }
        }
        super.onStart()
    }
}
