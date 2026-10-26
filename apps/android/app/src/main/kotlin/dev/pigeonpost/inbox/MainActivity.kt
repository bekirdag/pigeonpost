package dev.pigeonpost.inbox

import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.os.Build
import android.Manifest
import android.provider.Settings
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.activity.result.PickVisualMediaRequest
import androidx.activity.result.contract.ActivityResultContracts
import androidx.browser.customtabs.CustomTabsIntent
import androidx.core.content.FileProvider
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.lifecycleScope
import com.journeyapps.barcodescanner.ScanContract
import com.journeyapps.barcodescanner.ScanOptions
import dev.pigeonpost.core.Attachment
import dev.pigeonpost.core.AttachmentKinds
import dev.pigeonpost.inbox.calls.CallFrame
import dev.pigeonpost.inbox.calls.CallWindow
import dev.pigeonpost.core.verifiedSignInUrl
import dev.pigeonpost.inbox.ui.PigeonpostApp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch
import kotlinx.coroutines.flow.combine
import kotlinx.coroutines.flow.distinctUntilChanged
import dev.pigeonpost.inbox.push.PushNotifications
import dev.pigeonpost.inbox.calls.IncomingCalls

class MainActivity : ComponentActivity() {
    private val model: InboxViewModel by lazy {
        ViewModelProvider(this, object : ViewModelProvider.Factory {
            @Suppress("UNCHECKED_CAST")
            override fun <T : ViewModel> create(modelClass: Class<T>): T = InboxViewModel(application,
                Development.graph(application, intent) ?: (application as PigeonpostApplication).graph,
                Development.calls(intent), Development.appearance(intent)) as T
        })[InboxViewModel::class.java]
    }
    private val authorization = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { model.completeSignIn(it.data) }
    private val notifications = registerForActivityResult(ActivityResultContracts.RequestPermission()) { model.refreshPushRegistration(); offerFullScreenCalls() }
    private val documents = registerForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { model.attach(it) }
    private val photos = registerForActivityResult(ActivityResultContracts.PickMultipleVisualMedia(8)) { model.attach(it) }
    // Profile pictures: one photo from the picker, or a new one from the camera (written to a private cache file).
    private val picture = registerForActivityResult(ActivityResultContracts.PickVisualMedia()) { model.pictureChosen(it) }
    private var cameraFile: java.io.File? = null
    private val camera = registerForActivityResult(ActivityResultContracts.TakePicture()) { taken ->
        val file = cameraFile; cameraFile = null
        if (taken && file != null) model.pictureChosen(Uri.fromFile(file)) else file?.delete()
    }
    private val cameraPermission = registerForActivityResult(ActivityResultContracts.RequestPermission()) { granted ->
        if (granted) startCamera() else model.inbox.showError("Allow the camera in Android settings to take a picture.")
    }
    private fun takePicture() {
        if (checkSelfPermission(Manifest.permission.CAMERA) == android.content.pm.PackageManager.PERMISSION_GRANTED) startCamera()
        else cameraPermission.launch(Manifest.permission.CAMERA)
    }
    private fun startCamera() {
        val dir = java.io.File(cacheDir, "camera").apply { deleteRecursively(); mkdirs() }
        val file = java.io.File(dir, "picture.jpg")
        cameraFile = file
        try { camera.launch(FileProvider.getUriForFile(this, "$packageName.files", file)) }
        catch (_: ActivityNotFoundException) { model.inbox.showError("No camera app is available.") }
    }
    private val save = registerForActivityResult(ActivityResultContracts.CreateDocument("application/octet-stream")) { uri ->
        val file = model.pendingSave; model.pendingSave = null
        if (uri != null && file != null) lifecycleScope.launch { try { model.files.save(file, uri) } catch (failure: Exception) { report(failure) } }
    }
    private var permissionsAnswer: ((Map<String, Boolean>) -> Unit)? = null
    private val callPermissions = registerForActivityResult(ActivityResultContracts.RequestMultiplePermissions()) { result ->
        val answer = permissionsAnswer; permissionsAnswer = null; answer?.invoke(result)
    }
    private var callFrame: CallFrame? = null
    private val scanner = registerForActivityResult(ScanContract()) { result ->
        result.contents?.let { text ->
            val safe = verifiedSignInUrl(text)
            if (safe == null) model.inbox.showError("This is not a Pigeonpost sign-in code.")
            else openLink(safe)
        }
    }
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState); enableEdgeToEdge()
        Development.rememberHarness(this, intent)
        setContent { PigeonpostApp(model,
            signIn = { provider, other -> lifecycleScope.launch { model.session.begin(provider, other)?.let { intent ->
                try { authorization.launch(intent) }
                catch (_: ActivityNotFoundException) { model.session.cancel(); model.inbox.showError("Install a browser to sign in.") }
            } } },
            chooseFile = { model.chooseAttachments(); documents.launch(arrayOf("*/*")) },
            choosePhoto = { model.chooseAttachments(); photos.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageAndVideo)) },
            scan = { scanner.launch(ScanOptions().setDesiredBarcodeFormats(ScanOptions.QR_CODE).setPrompt("Scan a Pigeonpost sign-in code").setBeepEnabled(false).setOrientationLocked(false)) },
            attachment = ::attachment,
            openLink = ::openLink, notificationSettings = ::notificationSettings,
            fullScreenCalls = { if (model.graph.fixtures || IncomingCalls.canUseFullScreen(this)) null else ::fullScreenCallSettings },
            choosePicture = { picture.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly)) },
            takePicture = ::takePicture) }
        if (savedInstanceState == null) openNotification(intent)
        if (savedInstanceState == null) model.openGroupLink(intent.data)
        Development.started()
        model.calls?.let { host ->
            host.window = object : CallWindow {
                override fun requestPermissions(permissions: Array<String>, done: (Map<String, Boolean>) -> Unit) {
                    // One question at a time; a second request while the first is open is answered by the first.
                    val previous = permissionsAnswer
                    permissionsAnswer = { result -> previous?.invoke(result); done(result) }
                    if (previous == null) callPermissions.launch(permissions)
                }
                override fun openExternal(uri: Uri) = openLink(uri.toString())
                override fun toast(text: String) = model.inbox.showError(text)
            }
            callFrame = CallFrame(this, host)
            onBackPressedDispatcher.addCallback(this, callFrame!!.back)
        }
        if (savedInstanceState == null) answerCall(intent)
        lifecycleScope.launch {
            combine(model.session.state, model.inbox.state) { session, state -> session.signedIn && session.termsAccepted && state.acting != null }
                .distinctUntilChanged().collect { ready ->
                    if (ready && !model.graph.fixtures && Build.VERSION.SDK_INT >= 33) {
                        val preferences = getSharedPreferences("push", MODE_PRIVATE)
                        if (!preferences.getBoolean("asked_permission", false) && !PushNotifications.allowed(this@MainActivity)) {
                            preferences.edit().putBoolean("asked_permission", true).apply()
                            notifications.launch(Manifest.permission.POST_NOTIFICATIONS)
                        } else offerFullScreenCalls()
                    }
                }
        }
    }
    override fun onDestroy() {
        callFrame?.detach(); callFrame = null
        model.calls?.window = null
        super.onDestroy()
    }
    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent); setIntent(intent)
        openNotification(intent)
        answerCall(intent)
    }
    private fun openNotification(intent: Intent) {
        val payload = buildMap {
            intent.getStringExtra("push_identity")?.let { put("identity", it) }
            intent.getStringExtra("push_type")?.let { put("type", it) }
            for (key in listOf("calendar_event_id", "calendar_occurrence_id")) intent.getStringExtra(key)?.let { put(key, it) }
            val legacy = intent.data?.takeIf { it.scheme == "pigeonpost-notification" }?.lastPathSegment
            (intent.getStringExtra("push_message_id") ?: legacy)?.let { put("message_id", it) }
        }
        if (dev.pigeonpost.core.calendar.CalendarNotice.matches(payload)) model.openCalendarNotice(payload)
        else model.openNotification(intent.getStringExtra("push_identity"), intent.getStringExtra("push_peer"))
    }
    override fun onStart() {
        super.onStart(); PushNotifications.foreground = true; model.setActive(true); model.billing?.attach(this)
        model.calls?.setVisible(true); IncomingCalls.foreground(this)
    }
    override fun onResume() { super.onResume(); model.refreshPushRegistration(); if (model.session.state.value.signedIn) model.paidHandles?.restore() }
    override fun onStop() {
        PushNotifications.foreground = false; model.billing?.detach(this); model.setActive(false)
        model.calls?.setVisible(false); IncomingCalls.background(this)
        super.onStop()
    }
    /** Answer from the call notification or the incoming-call screen: the call page accepts with the ring token. */
    private fun answerCall(intent: Intent?) {
        if (intent?.action != IncomingCalls.ACTION_ANSWER || intent.flags and Intent.FLAG_ACTIVITY_LAUNCHED_FROM_HISTORY != 0) return
        val ring = IncomingCalls.fromIntent(intent)
        setIntent(Intent(intent).setAction(null))
        if (ring == null || ring.expiresAt * 1000 < System.currentTimeMillis() - 5000 || ring.identity != IncomingCalls.identity(this)) return
        IncomingCalls.answered(this, ring)
        model.calls?.answer(ring.room, ring.ringToken, ring.kind)
    }
    /** Android 14+: full-screen call screens can be switched off; ask once, then Settings keeps the way back. */
    private fun offerFullScreenCalls() {
        if (model.graph.fixtures || IncomingCalls.canUseFullScreen(this) || !IncomingCalls.allowed(this)) return
        val preferences = getSharedPreferences("push", MODE_PRIVATE)
        if (preferences.getBoolean("asked_full_screen", false)) return
        preferences.edit().putBoolean("asked_full_screen", true).apply()
        android.app.AlertDialog.Builder(this).setTitle("Show calls on the lock screen")
            .setMessage("Allow Pigeonpost to show incoming calls full screen, like a phone call. Without it, calls ring as a notification.")
            .setPositiveButton("Allow") { _, _ -> fullScreenCallSettings() }.setNegativeButton("Not now", null).show()
    }
    private fun fullScreenCallSettings() {
        val intent = if (Build.VERSION.SDK_INT >= 34) Intent(Settings.ACTION_MANAGE_APP_USE_FULL_SCREEN_INTENT, Uri.parse("package:$packageName"))
            else Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).putExtra(Settings.EXTRA_APP_PACKAGE, packageName)
        try { startActivity(intent) } catch (_: ActivityNotFoundException) { notificationSettings() }
    }
    private fun notificationSettings() {
        startActivity(Intent(Settings.ACTION_APP_NOTIFICATION_SETTINGS).putExtra(Settings.EXTRA_APP_PACKAGE, packageName))
    }
    private fun attachment(value: Attachment, action: String) {
        val identity = model.inbox.state.value.acting?.address ?: return
        lifecycleScope.launch {
            try {
                val file = model.fetch(identity, value)
                if (identity != model.inbox.state.value.acting?.address || !model.session.state.value.signedIn) return@launch
                if (action == "download" && AttachmentKinds.savesToGallery(value.filename, value.mediaType) && Build.VERSION.SDK_INT >= 29) {
                    // Photos and videos go to the gallery; nothing else is written there.
                    try { model.files.saveToGallery(file, value.filename, AttachmentKinds.type(value.filename, value.mediaType)); model.inbox.showError("Saved to your photos.") }
                    finally { file.parentFile?.deleteRecursively() }
                    return@launch
                }
                if (action == "save" || action == "download") { model.pendingSave = file; save.launch(file.name); return@launch }
                val uri = FileProvider.getUriForFile(this@MainActivity, "$packageName.files", file)
                val intent = if (action == "share") Intent(Intent.ACTION_SEND).setType(value.mediaType).putExtra(Intent.EXTRA_STREAM, uri)
                    else Intent(Intent.ACTION_VIEW).setDataAndType(uri, value.mediaType)
                intent.addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
                intent.clipData = ClipData.newRawUri(value.filename, uri)
                startActivity(Intent.createChooser(intent, if (action == "share") "Share attachment" else "Open attachment"))
            } catch (failure: Exception) { report(failure) }
        }
    }
    private fun openLink(url: String) {
        val uri = Uri.parse(url)
        if (uri.scheme !in setOf("https", "http") || uri.host.isNullOrBlank()) return
        try { CustomTabsIntent.Builder().setShowTitle(true).build().launchUrl(this, uri) }
        catch (_: ActivityNotFoundException) { model.inbox.showError("Install a browser to open this link.") }
    }
    private fun report(failure: Exception) {
        if (failure is CancellationException) throw failure
        model.inbox.showError(failure.message ?: "Could not open the attachment.")
    }
}
