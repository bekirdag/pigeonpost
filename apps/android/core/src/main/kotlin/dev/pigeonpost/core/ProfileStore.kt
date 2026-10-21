package dev.pigeonpost.core

import kotlinx.coroutines.CancellationException
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Job
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import kotlinx.coroutines.launch

data class ProfileState(
    val mailbox: String? = null,
    val loading: Boolean = false,
    val avatar: AvatarInfo? = null,
    /** Who may see it; applied with the next upload when there is no picture yet. Default "people I know". */
    val visibility: String = "known",
    val note: String? = null,
    val saving: Boolean = false,
    val error: String? = null,
)

/**
 * Settings → Profile picture (SDS §16.1), per mailbox: load the picture, upload a cropped photo
 * (optionally for all of the account's mailboxes), change who may see it, or remove it.
 */
class ProfileStore(private val api: PostboxApi, private val scope: CoroutineScope, private val changed: (Collection<String>) -> Unit = {}) {
    private val mutable = MutableStateFlow(ProfileState())
    val state = mutable.asStateFlow()
    private var job: Job? = null

    fun load(mailbox: String) {
        job?.cancel()
        mutable.value = ProfileState(mailbox = mailbox, loading = true, note = "Loading…")
        job = scope.launch {
            try {
                val avatar = api.ownAvatar(mailbox)
                if (state.value.mailbox != mailbox) return@launch
                mutable.update { it.copy(loading = false, avatar = avatar, visibility = avatar?.visibility ?: it.visibility,
                    note = if (avatar != null) "This picture belongs to the mailbox you are using now." else "No picture yet. People see your initials.") }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) { if (state.value.mailbox == mailbox) mutable.update { it.copy(loading = false, note = "Could not load your picture. Try again later.") } }
        }
    }

    fun setVisibility(value: String) {
        val mailbox = state.value.mailbox ?: return
        val previous = state.value.visibility
        mutable.update { it.copy(visibility = value, error = null) }
        if (state.value.avatar == null) return // applied with the next upload
        scope.launch {
            try {
                val saved = api.setAvatarVisibility(mailbox, value)
                if (state.value.mailbox != mailbox) return@launch
                mutable.update { it.copy(avatar = saved ?: it.avatar, note = "Picture visibility saved.") }
                changed(listOf(mailbox))
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) { if (state.value.mailbox == mailbox) mutable.update { it.copy(visibility = previous, error = "Could not change who can see your picture.") } }
        }
    }

    /** `image` is already the square crop, re-encoded on the device (no metadata leaves it). */
    fun upload(image: ByteArray, mediaType: String, side: Int, all: Boolean, everyMailbox: Collection<String>, done: () -> Unit = {}) {
        val mailbox = state.value.mailbox ?: return
        mutable.update { it.copy(saving = true, error = null) }
        scope.launch {
            try {
                val saved = api.setAvatar(mailbox, image, mediaType, CropRect(0, 0, side), state.value.visibility, all)
                if (state.value.mailbox != mailbox) return@launch
                mutable.update { it.copy(saving = false, avatar = saved ?: it.avatar, note = if (all) "Saved for all your mailboxes." else "Saved.") }
                changed(if (all) everyMailbox else listOf(mailbox))
                done()
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: ApiException) { mutable.update { it.copy(saving = false, error = avatarError(failure.status, failure.code)) } }
            catch (failure: Exception) { mutable.update { it.copy(saving = false, error = failure.message?.takeIf { failure is IllegalArgumentException } ?: "Could not reach your inbox. Please try again.") } }
        }
    }

    fun remove() {
        val mailbox = state.value.mailbox ?: return
        mutable.update { it.copy(saving = true, error = null) }
        scope.launch {
            try {
                api.removeAvatar(mailbox)
                if (state.value.mailbox != mailbox) return@launch
                mutable.update { it.copy(saving = false, avatar = null, note = "Picture removed. People see your initials.") }
                changed(listOf(mailbox))
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) { mutable.update { it.copy(saving = false, error = "Could not remove your picture.") } }
        }
    }

    fun reset() { job?.cancel(); mutable.value = ProfileState() }

    companion object {
        fun avatarError(status: Int, code: String?): String = when {
            status == 429 || code == "avatar_rate_limited" -> "You can change your picture five times a day. Try again tomorrow."
            status == 413 -> "That picture is too large. Choose one under 10 MB."
            status == 415 || status == 422 -> "That picture could not be used. Try a JPEG or PNG."
            else -> "Could not save your picture. Please try again."
        }
    }
}
