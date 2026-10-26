package dev.pigeonpost.inbox

import android.content.Context
import android.content.ContextWrapper
import androidx.test.core.app.ApplicationProvider
import androidx.test.ext.junit.runners.AndroidJUnit4
import dev.pigeonpost.core.AuthorizationLease
import dev.pigeonpost.core.AuthorizationRetired
import dev.pigeonpost.core.groups.GroupRecord
import dev.pigeonpost.inbox.groups.KeystoreGroupStorage
import kotlinx.coroutines.runBlocking
import org.junit.Assert.*
import org.junit.Test
import org.junit.runner.RunWith
import java.io.File
import java.util.UUID

@RunWith(AndroidJUnit4::class)
class GroupStorageAuthorizationTest {
    @Test fun aRetiredActorCannotOpenOverwriteOrDeleteTheCurrentKeystoreVault() = runBlocking {
        val context = ApplicationProvider.getApplicationContext<Context>()
        val directory = File(context.cacheDir, "group-authorization-${UUID.randomUUID()}").apply { mkdirs() }
        val isolated = object : ContextWrapper(context) { override fun getNoBackupFilesDir() = directory }
        try {
            val storage = KeystoreGroupStorage(isolated)
            val old = AuthorizationLease(); val current = AuthorizationLease()
            val first = GroupRecord("old-device", stateKey = "old-state-key")
            val next = GroupRecord("current-device", stateKey = "current-state-key")
            storage.save("/alice", first, old)
            old.retire(); storage.save("/alice", next, current)
            suspend fun refused(action: suspend () -> Unit) {
                try { action(); fail("Retired authorization reached the vault") }
                catch (_: AuthorizationRetired) { }
            }
            refused { storage.load("/alice", old) }
            refused { storage.save("/alice", first, old) }
            refused { storage.remove("/alice", old) }
            assertEquals(next, storage.load("/alice", current))
            val encrypted = File(directory, "groups").listFiles()!!.single().readBytes().toString(Charsets.ISO_8859_1)
            assertFalse(encrypted.contains(next.stateKey))
            storage.remove("/alice", current)
            assertNull(storage.load("/alice", current))
        } finally { directory.deleteRecursively() }
    }
}
