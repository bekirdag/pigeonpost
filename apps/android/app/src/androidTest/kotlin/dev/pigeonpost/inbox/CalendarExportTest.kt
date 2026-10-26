package dev.pigeonpost.inbox

import android.content.Context
import androidx.core.content.FileProvider
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.pigeonpost.core.*
import dev.pigeonpost.core.calendar.*
import kotlinx.coroutines.*
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File

@RunWith(AndroidJUnit4::class)
class CalendarExportTest {
    private val context get() = ApplicationProvider.getApplicationContext<Context>()
    private val ics = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nSUMMARY:Türkçe calendar\r\nEND:VCALENDAR\r\n".toByteArray()
    private suspend fun withExport(work: suspend (CalendarStore, CalendarIcsExport) -> Unit) {
        val base = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)
        val scope = AuthorizedStoreScope(base).apply { activate(AuthorizationLease()) }
        val transport = object : CalendarTransport {
            override suspend fun calendarRequest(mailbox: String, method: String, path: String, query: Map<String, String>, body: ByteArray?, mediaType: String): ByteArray {
                check(path.endsWith(".ics")); return ics.copyOf()
            }
        }
        val store = CalendarStore(transport, MemoryCalendarAttemptStorage(), scope, allowVolatileStorage = true)
        try {
            withTimeout(10_000) {
                val packet = withContext(Dispatchers.Main) {
                    store.onMailbox("/alice", setOf("calendar")); store.loadAttempts()!!.join()
                    val ready = CompletableDeferred<CalendarIcsExport>()
                    store.prepareIcsExport("evt_one", store.state.value.contextId) { ready.complete(it) }!!.join()
                    ready.await()
                }
                try { work(store, packet) } finally { packet.close() }
            }
        } finally { withContext(Dispatchers.Main) { store.reset(); scope.retire() }; base.cancel() }
    }
    @Test fun actualDocumentProviderReceivesExactProjectionWithoutAPrivateStagingFile() = runBlocking<Unit> {
        withExport { _, packet ->
            val dir = File(context.cacheDir, "received").apply { mkdirs() }
            val destination = File.createTempFile("calendar-export-test-", ".ics", dir)
            try {
                val uri = FileProvider.getUriForFile(context, "${context.packageName}.files", destination)
                withContext(Dispatchers.IO) { packet.write { context.contentResolver.openOutputStream(uri, "w")!! } }
                assertArrayEquals(ics, destination.readBytes())
            } finally { destination.delete() }
        }
    }
    @Test fun aLatePickerResultCannotTruncateAnExistingDocumentAfterMailboxSwitch() = runBlocking<Unit> {
        withExport { store, packet ->
            val dir = File(context.cacheDir, "received").apply { mkdirs() }
            val destination = File.createTempFile("calendar-export-retired-test-", ".ics", dir)
            try {
                destination.writeText("Existing document")
                val uri = FileProvider.getUriForFile(context, "${context.packageName}.files", destination)
                withContext(Dispatchers.Main) { store.onMailbox("/bob", setOf("calendar")) }
                try { withContext(Dispatchers.IO) { packet.write { context.contentResolver.openOutputStream(uri, "w")!! } }; fail("Expected a retired export") }
                catch (_: AuthorizationRetired) { }
                assertEquals("Existing document", destination.readText())
            } finally { destination.delete() }
        }
    }
}
