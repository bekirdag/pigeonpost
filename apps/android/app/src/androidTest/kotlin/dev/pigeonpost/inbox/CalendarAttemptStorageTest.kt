package dev.pigeonpost.inbox

import android.content.Context
import android.content.ContextWrapper
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.pigeonpost.core.AuthorizationLease
import dev.pigeonpost.core.AuthorizationRetired
import dev.pigeonpost.core.calendar.*
import dev.pigeonpost.inbox.calendar.KeystoreCalendarAttempts
import kotlinx.coroutines.runBlocking
import kotlinx.serialization.json.buildJsonObject
import kotlinx.serialization.json.put
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.util.UUID

@RunWith(AndroidJUnit4::class)
class CalendarAttemptStorageTest {
    private fun attempt(id: String = "fixture-attempt", mailbox: String = "/alice") = CalendarPendingAttempt(id, mailbox, CalendarAttemptKind.EVENT,
        CalendarWritePayload(buildJsonObject { put("request_key", id); put("title", "Private calendar fixture"); put("description", "İğdır exact retry ✓") }).bytes().toString(Charsets.UTF_8), 100)
    private fun isolated(work: suspend (KeystoreCalendarAttempts, File, Context) -> Unit) = runBlocking<Unit> {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val directory = File(context.cacheDir, "calendar-attempt-test-${UUID.randomUUID()}").apply { mkdirs() }
        val isolated = object : ContextWrapper(context) { override fun getNoBackupFilesDir() = directory }
        try { work(KeystoreCalendarAttempts(isolated), directory, isolated) }
        finally { directory.deleteRecursively() }
    }
    private fun files(directory: File) = File(directory, "calendar-attempts").listFiles()!!.filter { it.name.endsWith(".enc") }
    private suspend inline fun <reified T : Throwable> fails(noinline work: suspend () -> Unit): T {
        val failure = runCatching { work() }.exceptionOrNull()
        assertTrue("Expected ${T::class.simpleName}", failure is T)
        return failure as T
    }

    @Test fun exactRetrySurvivesAStorageRecreationWithoutPlaintextOnDisk() = isolated { storage, directory, context ->
        val value = attempt(); val lease = AuthorizationLease()
        storage.save("/alice", listOf(value), lease)
        val restored = KeystoreCalendarAttempts(context).load("/alice", lease).single()
        assertEquals(value, restored); assertArrayEquals(value.payload().bytes(), restored.payload().bytes())
        val file = files(directory).single()
        val bytes = file.readBytes(); val text = bytes.toString(Charsets.ISO_8859_1)
        assertEquals(1.toByte(), bytes[0])
        assertFalse(text.contains("Private calendar fixture")); assertFalse(text.contains("request_key"))
        storage.save("/alice", emptyList(), lease)
        assertTrue(KeystoreCalendarAttempts(context).load("/alice", lease).isEmpty())
    }
    @Test fun aRetiredActorCannotOpenReplaceOrEraseTheNewAttempt() = isolated { storage, _, _ ->
        val old = AuthorizationLease(); val current = AuthorizationLease()
        storage.save("/alice", listOf(attempt("old-attempt")), old); old.retire()
        val replacement = attempt("current-attempt")
        storage.save("/alice", listOf(replacement), current)
        fails<AuthorizationRetired> { storage.load("/alice", old) }
        fails<AuthorizationRetired> { storage.save("/alice", listOf(attempt("old-attempt")), old) }
        fails<AuthorizationRetired> { storage.save("/alice", emptyList(), old) }
        assertEquals(listOf(replacement), storage.load("/alice", current))
    }
    @Test fun tamperingIsUnreadableStateRatherThanAnEmptyOrDeletedOutbox() = isolated { storage, directory, _ ->
        val lease = AuthorizationLease()
        storage.save("/alice", listOf(attempt()), lease)
        val file = files(directory).single(); val corrupted = file.readBytes()
        corrupted[corrupted.lastIndex] = (corrupted.last().toInt() xor 1).toByte(); file.writeBytes(corrupted)
        val failure = fails<CalendarAttemptStorageException> { storage.load("/alice", lease) }
        assertNull(failure.cause); assertFalse(failure.message!!.contains("Private calendar fixture"))
        assertArrayEquals(corrupted, file.readBytes())
        fails<CalendarAttemptStorageException> { storage.load("/alice", lease) }
    }
    @Test fun mailboxAuthenticationPreventsSwappingTwoValidEncryptedFiles() = isolated { storage, directory, _ ->
        val lease = AuthorizationLease()
        storage.save("/alice", listOf(attempt("alice-attempt")), lease)
        storage.save("/bob", listOf(attempt("bob-attempt", "/bob")), lease)
        val files = files(directory); assertEquals(2, files.size)
        val first = files[0].readBytes(); val second = files[1].readBytes()
        files[0].writeBytes(second); files[1].writeBytes(first)
        fails<CalendarAttemptStorageException> { storage.load("/alice", lease) }
        fails<CalendarAttemptStorageException> { storage.load("/bob", lease) }
    }
    @Test fun anAtomicFileBackupIsRecoveredBeforeDecidingThereIsNoPendingWrite() = isolated { storage, directory, context ->
        val lease = AuthorizationLease(); val value = attempt()
        storage.save("/alice", listOf(value), lease)
        val file = files(directory).single()
        file.copyTo(File(file.path + ".bak")); assertTrue(file.delete())
        assertEquals(listOf(value), KeystoreCalendarAttempts(context).load("/alice", lease))
    }
}
