package dev.pigeonpost.inbox.calls

import android.annotation.SuppressLint
import android.content.Context
import android.view.MotionEvent
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.widget.FrameLayout
import androidx.activity.ComponentActivity
import androidx.activity.OnBackPressedCallback
import androidx.core.view.ViewCompat
import androidx.core.view.WindowInsetsCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.lifecycleScope
import androidx.lifecycle.repeatOnLifecycle
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.launch

/**
 * The call, over the app: the whole screen (inside the system bars), a 64 dp strip at the top while
 * the person reads mail during a call, or nothing that can be seen or touched. The view sits above the
 * Compose content in the activity's own frame, so a hidden call lets every touch through.
 */
class CallFrame(private val activity: ComponentActivity, private val host: CallHost) {
    private class Container(context: Context) : FrameLayout(context) {
        var passThrough = true
        /** The strips behind the status and navigation bars, painted during a full-screen call. */
        var bars = IntArray(2)
        private val paint = android.graphics.Paint().apply { color = 0xFF0D0F12.toInt() }
        init { setWillNotDraw(false) }
        override fun onDraw(canvas: android.graphics.Canvas) {
            if (bars[0] > 0) canvas.drawRect(0f, 0f, width.toFloat(), bars[0].toFloat(), paint)
            if (bars[1] > 0) canvas.drawRect(0f, (height - bars[1]).toFloat(), width.toFloat(), height.toFloat(), paint)
        }
        @SuppressLint("ClickableViewAccessibility")
        override fun dispatchTouchEvent(event: MotionEvent) = if (passThrough) false else super.dispatchTouchEvent(event)
        override fun onInterceptTouchEvent(event: MotionEvent) = passThrough
    }

    private val container = Container(activity)
    private var insets = WindowInsetsCompat.CONSUMED
    /** A full-screen call or ring keeps Back from closing the app underneath it. */
    val back = object : OnBackPressedCallback(false) { override fun handleOnBackPressed() {} }

    init {
        host.context.baseContext = activity
        activity.addContentView(container, ViewGroup.LayoutParams(ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.MATCH_PARENT))
        ViewCompat.setOnApplyWindowInsetsListener(container) { _, value -> insets = value; layout(host.mode.value); value }
        activity.lifecycleScope.launch {
            activity.repeatOnLifecycle(Lifecycle.State.CREATED) {
                combine(host.mode, host.rebuilt) { mode, _ -> mode }.collect { mode -> adopt(); layout(mode) }
            }
        }
    }

    private fun adopt() {
        val web = host.webView
        if (web.parent === container) return
        (web.parent as? ViewGroup)?.removeView(web)
        container.addView(web, FrameLayout.LayoutParams(FrameLayout.LayoutParams.MATCH_PARENT, FrameLayout.LayoutParams.MATCH_PARENT))
    }

    private fun layout(mode: CallHost.Mode) {
        val web = host.webView
        val bars = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
        val strip = (64 * activity.resources.displayMetrics.density).toInt()
        container.passThrough = mode == CallHost.Mode.HIDDEN
        container.importantForAccessibility = if (mode == CallHost.Mode.HIDDEN) View.IMPORTANT_FOR_ACCESSIBILITY_NO_HIDE_DESCENDANTS else View.IMPORTANT_FOR_ACCESSIBILITY_AUTO
        container.layoutParams = (container.layoutParams as FrameLayout.LayoutParams).apply {
            height = if (mode == CallHost.Mode.MINI) bars.top + strip else ViewGroup.LayoutParams.MATCH_PARENT
        }
        (web.layoutParams as? FrameLayout.LayoutParams)?.let { params ->
            params.setMargins(bars.left, bars.top, bars.right, if (mode == CallHost.Mode.MINI) 0 else bars.bottom)
            params.height = if (mode == CallHost.Mode.MINI) strip else FrameLayout.LayoutParams.MATCH_PARENT
            web.layoutParams = params
        }
        web.alpha = if (mode == CallHost.Mode.HIDDEN) 0f else 1f
        // Behind the system bars during a full-screen call: the call's own dark ground.
        container.bars = if (mode == CallHost.Mode.FULL) intArrayOf(bars.top, bars.bottom) else IntArray(2)
        container.invalidate()
        container.requestLayout()
        back.isEnabled = mode == CallHost.Mode.FULL
        // A call keeps the screen awake, as the phone app does.
        if (mode == CallHost.Mode.HIDDEN) activity.window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        else activity.window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
    }

    /** The view outlives this activity (rotation): hand it back to the application context. */
    fun detach() {
        container.removeAllViews()
        host.context.baseContext = activity.applicationContext
    }
}
