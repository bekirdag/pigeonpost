package dev.pigeonpost.inbox

import android.graphics.Color
import androidx.test.platform.app.InstrumentationRegistry
import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

@RunWith(AndroidJUnit4::class)
class AttachmentPreviewTest {
    @Test fun nativeRenderersProduceImageVideoAndPdfPixels() {
        val instrumentation = InstrumentationRegistry.getInstrumentation()
        val folder = File(instrumentation.targetContext.cacheDir, "preview-render-test").apply { mkdirs() }
        try {
            for ((name, mime, dominant) in listOf(Triple("preview.png", "image/png", "red"),
                Triple("preview.mp4", "video/mp4", "blue"), Triple("preview.pdf", "application/pdf", "green"))) {
                val file = File(folder, name)
                instrumentation.context.assets.open(name).use { input -> file.outputStream().use { input.copyTo(it) } }
                val bitmap = AttachmentThumbnails.render(file, name, mime)?.bitmap
                assertNotNull("$name should render real pixels", bitmap)
                bitmap!!
                assertTrue(bitmap.width <= 440 && bitmap.height <= 300)
                val color = bitmap.getPixel(bitmap.width / 2, bitmap.height / 2)
                when (dominant) {
                    "red" -> assertTrue(Color.red(color) > Color.green(color) + 80)
                    "blue" -> assertTrue(Color.blue(color) > Color.red(color) + 80)
                    "green" -> assertTrue(Color.green(color) > Color.blue(color) + 50)
                }
                bitmap.recycle()
            }
            assertTrue(AttachmentThumbnails.supports("photo.png", "application/octet-stream"))
            assertFalse(AttachmentThumbnails.supports("photo.png", "text/html"))
            assertFalse(AttachmentThumbnails.supports("photo.svg", "image/svg+xml"))
        } finally { folder.deleteRecursively() }
    }
}
