package dev.pigeonpost.inbox

import android.content.Context
import android.os.Build
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.pigeonpost.core.meetings.MeetingBrief
import dev.pigeonpost.core.meetings.MeetingDetail
import dev.pigeonpost.core.meetings.MeetingNotesCrypto
import dev.pigeonpost.core.meetings.MeetingNotesError
import dev.pigeonpost.inbox.meetings.KeystoreNotesKeys
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.security.KeyStore

/** The notes key in the Android Keystore, both ways it is kept: agreement inside the Keystore, and the Android 8–11 wrapped key. */
@RunWith(AndroidJUnit4::class)
class NotesKeystoreTest {
    private val context get() = ApplicationProvider.getApplicationContext<Context>()

    private fun roundTrip(keys: KeystoreNotesKeys) {
        val key = keys.current()
        val id = "mtg_" + "k".repeat(26)
        val sealed = MeetingNotesCrypto.seal(id, """{"title":"On device","summary":"Opened here."}""".toByteArray(), listOf(key.keyId to MeetingNotesCrypto.unb64(key.publicKey)))
        val detail = MeetingDetail(id, state = "ready", alg = "A256GCM", iv = sealed.iv, ciphertext = sealed.ciphertext, keys = sealed.wraps)
        assertEquals("Opened here.", MeetingBrief.parse(MeetingNotesCrypto.open(detail, key)).summary)
        // Reloaded from storage, the same key opens it.
        val again = KeystoreNotesKeys(context, NAME, agreeInKeystore = keys.hardwareBacked).current()
        assertEquals(key.keyId, again.keyId); assertEquals(key.publicKey, again.publicKey)
        assertEquals("On device", MeetingBrief.parse(MeetingNotesCrypto.open(detail, again)).title)
        // Signing out destroys it: a new key with a new id, which cannot open the old notes.
        keys.destroy()
        val fresh = KeystoreNotesKeys(context, NAME, agreeInKeystore = keys.hardwareBacked).current()
        assertNotEquals(key.keyId, fresh.keyId)
        assertEquals("no_key_for_device", assertThrows(MeetingNotesError::class.java) { MeetingNotesCrypto.open(detail, fresh) }.code)
        KeystoreNotesKeys(context, NAME).destroy()
    }

    @Test fun agreementRunsInsideTheKeystore() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) return
        val keys = KeystoreNotesKeys(context, NAME).also { it.destroy() }
        keys.current()
        assertTrue(keys.hardwareBacked)
        val entry = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }.getEntry("pigeonpost.$NAME.agree.v1", null) as KeyStore.PrivateKeyEntry
        assertNull("the private half is not exportable", entry.privateKey.encoded)
        roundTrip(keys)
    }

    @Test fun theOlderAndroidPathKeepsOnlyCiphertext() {
        val keys = KeystoreNotesKeys(context, NAME, agreeInKeystore = false).also { it.destroy() }
        keys.current()
        assertFalse(keys.hardwareBacked)
        val stored = java.io.File(context.noBackupFilesDir, "$NAME-key.enc").readBytes()
        assertEquals(1.toByte(), stored[0])
        roundTrip(keys)
    }

    companion object { const val NAME = "test-meeting-notes" }
}
