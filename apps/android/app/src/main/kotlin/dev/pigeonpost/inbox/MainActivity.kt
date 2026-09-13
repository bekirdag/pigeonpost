package dev.pigeonpost.inbox

import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.Intent
import android.net.Uri
import android.os.Bundle
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
import dev.pigeonpost.core.verifiedSignInUrl
import dev.pigeonpost.inbox.ui.PigeonpostApp
import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.launch

class MainActivity : ComponentActivity() {
    private val model: InboxViewModel by lazy {
        ViewModelProvider(this, object : ViewModelProvider.Factory {
            @Suppress("UNCHECKED_CAST")
            override fun <T : ViewModel> create(modelClass: Class<T>): T = InboxViewModel(application,
                Development.graph(application, intent) ?: (application as PigeonpostApplication).graph) as T
        })[InboxViewModel::class.java]
    }
    private val authorization = registerForActivityResult(ActivityResultContracts.StartActivityForResult()) { model.completeSignIn(it.data) }
    private val documents = registerForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { model.attach(it) }
    private val photos = registerForActivityResult(ActivityResultContracts.PickMultipleVisualMedia(8)) { model.attach(it) }
    private val save = registerForActivityResult(ActivityResultContracts.CreateDocument("application/octet-stream")) { uri ->
        val file = model.pendingSave; model.pendingSave = null
        if (uri != null && file != null) lifecycleScope.launch { try { model.files.save(file, uri) } catch (failure: Exception) { report(failure) } }
    }
    private val scanner = registerForActivityResult(ScanContract()) { result ->
        result.contents?.let { text ->
            val safe = verifiedSignInUrl(text)
            if (safe == null) model.inbox.showError("This is not a Pigeonpost sign-in code.")
            else openLink(safe)
        }
    }
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState); enableEdgeToEdge()
        setContent { PigeonpostApp(model,
            signIn = { provider, other -> lifecycleScope.launch { model.session.begin(provider, other)?.let { intent ->
                try { authorization.launch(intent) }
                catch (_: ActivityNotFoundException) { model.session.cancel(); model.inbox.showError("Install a browser to sign in.") }
            } } },
            chooseFile = { model.chooseAttachments(); documents.launch(arrayOf("*/*")) },
            choosePhoto = { model.chooseAttachments(); photos.launch(PickVisualMediaRequest(ActivityResultContracts.PickVisualMedia.ImageOnly)) },
            scan = { scanner.launch(ScanOptions().setDesiredBarcodeFormats(ScanOptions.QR_CODE).setPrompt("Scan a Pigeonpost sign-in code").setBeepEnabled(false).setOrientationLocked(false)) },
            attachment = ::attachment,
            openLink = ::openLink) }
    }
    override fun onStart() { super.onStart(); model.inbox.setActive(true); model.billing?.attach(this) }
    override fun onResume() { super.onResume(); if (model.session.state.value.signedIn) model.paidHandles?.restore() }
    override fun onStop() { model.billing?.detach(this); model.inbox.setActive(false); super.onStop() }
    private fun attachment(value: Attachment, action: String) {
        val identity = model.inbox.state.value.acting?.address ?: return
        lifecycleScope.launch {
            try {
                val file = model.files.download(identity, value)
                if (identity != model.inbox.state.value.acting?.address || !model.session.state.value.signedIn) return@launch
                if (action == "save") { model.pendingSave = file; save.launch(file.name); return@launch }
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
