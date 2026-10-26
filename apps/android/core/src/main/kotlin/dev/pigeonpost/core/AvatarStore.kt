package dev.pigeonpost.core

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Deferred
import kotlinx.coroutines.Job
import kotlinx.coroutines.async
import kotlinx.coroutines.delay
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

/**
 * Profile pictures (SDS §16), as the web inbox's `Avatars`: addresses are looked up in batches of
 * up to 100, the answer (a picture's hash and path, or null for none / not visible) is cached per
 * address, and the bytes are fetched once per hash with the bearer token, since a "people I know"
 * picture is not public. Initials stay underneath as the fallback.
 */
class AvatarStore(private val api: PostboxApi, private val scope: CoroutineScope) {
    /** Address or handle → picture, or null when there is none this account may see. Absent = not asked yet. */
    private val mutable = MutableStateFlow<Map<String, AvatarInfo?>>(emptyMap())
    val pictures: StateFlow<Map<String, AvatarInfo?>> = mutable.asStateFlow()
    private val mutableEnabled = MutableStateFlow(false)
    /** Already-visible faces retry their lookup when capability discovery enables pictures. */
    val availability: StateFlow<Boolean> = mutableEnabled.asStateFlow()
    private val queue = linkedSetOf<String>()
    private var flushJob: Job? = null
    private var generation = 0L
    private val bytes = object : LinkedHashMap<String, ByteArray>(32, 0.75f, true) {
        override fun removeEldestEntry(eldest: MutableMap.MutableEntry<String, ByteArray>?) = size > 96
    }
    private val loading = mutableMapOf<String, Deferred<ByteArray?>>()
    @Volatile var enabled = false
        private set

    fun enable(on: Boolean) {
        if (on == enabled) return
        reset()
        enabled = on
        mutableEnabled.value = on
    }

    fun reset() {
        generation++
        enabled = false
        mutableEnabled.value = false
        flushJob?.cancel(); flushJob = null
        synchronized(queue) { queue.clear() }
        synchronized(bytes) { bytes.clear() }
        synchronized(loading) { loading.values.forEach { it.cancel() }; loading.clear() }
        mutable.value = emptyMap()
    }

    /** Ask for `key`'s picture if nobody has yet. Cheap to call from every face on screen. */
    fun want(key: String?) {
        if (!enabled || key == null || !key.startsWith('/') || key.endsWith("/*") || key.contains(',') || key.length > 300) return
        if (key in mutable.value) return
        // The first address in an empty queue starts a batch; the rest ride along with it.
        val start = synchronized(queue) { val first = queue.isEmpty(); queue.add(key) && first }
        if (start) {
            val run = generation
            flushJob = scope.launch { delay(30); flush(run) }
        }
    }

    private suspend fun flush(run: Long) {
        while (run == generation) {
            val keys = synchronized(queue) { queue.take(100) }
            if (keys.isEmpty()) return
            // Marked as "none" first, so a failed lookup is not retried in a loop.
            mutable.update { it + keys.associateWith { null } }
            val found = try { api.avatars(keys) }
                catch (cancelled: CancellationException) { throw cancelled }
                catch (_: Exception) { synchronized(queue) { queue.removeAll(keys.toSet()) }; continue }
            if (run != generation) return
            mutable.update { old -> old + keys.associateWith { key -> found[key]?.takeIf { it.path() != null } } }
            synchronized(queue) { queue.removeAll(keys.toSet()) }
        }
    }

    /** The picture's bytes, fetched once per hash however many addresses share it. */
    suspend fun bytes(picture: AvatarInfo, large: Boolean = false): ByteArray? {
        val path = picture.path(large) ?: return null
        val key = picture.cacheKey + if (large) ":large" else ""
        synchronized(bytes) { bytes[key] }?.let { return it }
        val run = generation
        val job = synchronized(loading) {
            loading.getOrPut(key) {
                scope.async {
                    try { api.avatarBytes(path).takeIf { it.isNotEmpty() } }
                    catch (cancelled: CancellationException) { throw cancelled }
                    catch (_: Exception) { null }
                }
            }
        }
        val result = try { job.await() } finally { synchronized(loading) { if (loading[key] === job) loading.remove(key) } }
        if (result != null && run == generation) synchronized(bytes) { bytes[key] = result }
        return result
    }

    /** A picture changed (a `profile.avatar` event, or this account saved one): look again. */
    fun forget(keys: Collection<String?>) {
        val drop = keys.filterNotNull().toSet()
        if (drop.isEmpty()) return
        mutable.update { it - drop }
    }
}
