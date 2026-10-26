package dev.pigeonpost.core.groups

import dev.pigeonpost.core.AuthorizationContext
import dev.pigeonpost.core.AuthorizationLease
import dev.pigeonpost.core.AuthorizationRetired
import kotlinx.coroutines.*
import kotlinx.serialization.json.JsonElement
import org.junit.Assert.*
import org.junit.Test

class GroupAuthorizationTest {
    private val mailbox = "/alice"
    private class Transport : GroupTransport {
        var calls = 0
        var result: suspend () -> String = { "{}" }
        override suspend fun groupJson(mailbox: String, method: String, path: String, body: JsonElement?, device: String?): String { calls++; return result() }
        override suspend fun groupUpload(mailbox: String, path: String, bytes: ByteArray, device: String): String { calls++; return "{}" }
        override suspend fun groupDownload(mailbox: String, path: String, device: String?, maximumBytes: Long): ByteArray { calls++; return byteArrayOf() }
        override suspend fun groupCall(groupId: String, from: String): GroupCallState? { calls++; return null }
    }
    private suspend fun refused(work: suspend () -> Unit) {
        try { work(); fail("A retired or unrelated actor was accepted") }
        catch (_: AuthorizationRetired) { }
    }

    @Test fun canonicalMailboxAppliesToMetadataMessagesFilesAndCalls() = runBlocking {
        val base = Transport(); val lease = AuthorizationLease()
        val api = AuthorizedGroupTransport(base, mailbox, lease)
        val other = "/bob"
        refused { api.groupJson(other, "POST", "/v1/mls/devices") }
        refused { api.groupUpload(other, "/v1/groups/grp_a/attachments", byteArrayOf(1), "device") }
        refused { api.groupDownload(other, "/v1/groups/grp_a/attachments/x", "device", 10) }
        refused { api.groupCall("grp_a", other) }
        assertEquals(0, base.calls)
        api.groupJson(mailbox, "GET", "/v1/groups")
        assertEquals(1, base.calls)
    }

    @Test fun aRetiredMailboxTaskCannotBorrowItsSiblingEvenOnTheSameLogin() = runBlocking {
        val root = AuthorizationLease(); val old = root.child(); val current = root.child()
        val base = Transport(); val api = AuthorizedGroupTransport(base, mailbox, current)
        old.retire()
        withContext(AuthorizationContext(old)) { refused { api.groupJson(mailbox, "POST", "/v1/groups") } }
        withContext(AuthorizationContext(AuthorizationLease())) { refused { api.groupJson(mailbox, "POST", "/v1/groups") } }
        assertEquals(0, base.calls)
        withContext(AuthorizationContext(root)) { api.groupJson(mailbox, "GET", "/v1/groups") }
        assertEquals(1, base.calls)
    }

    @Test fun aLateMetadataReplyIsNotReturnedToTheRetiredClient() = runBlocking {
        val base = Transport(); val lease = AuthorizationLease()
        val entered = CompletableDeferred<Unit>(); val release = CompletableDeferred<Unit>()
        base.result = { entered.complete(Unit); release.await(); "private old membership" }
        val api = AuthorizedGroupTransport(base, mailbox, lease)
        val pending = async { runCatching { api.groupJson(mailbox, "GET", "/v1/groups") } }
        entered.await(); lease.retire(); release.complete(Unit)
        assertTrue(pending.await().exceptionOrNull() is AuthorizationRetired)
        assertEquals(1, base.calls)
    }

    @Test fun retiredStorageCannotReadOverwriteOrEraseTheReplacementRecord() = runBlocking {
        val storage = MemoryGroupStorage(); val old = AuthorizationLease(); val current = AuthorizationLease()
        val first = GroupRecord("old-device", stateKey = "old-key")
        val next = GroupRecord("new-device", stateKey = "new-key")
        storage.save(mailbox, first, old); old.retire(); storage.save(mailbox, next, current)
        refused { storage.load(mailbox, old) }
        refused { storage.save(mailbox, first, old) }
        refused { storage.remove(mailbox, old) }
        assertEquals(next, storage.load(mailbox, current))
        storage.remove(mailbox, current); assertNull(storage.load(mailbox, current))
    }
}
