package dev.pigeonpost.core.contacts

import dev.pigeonpost.core.*
import kotlinx.coroutines.*
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.update
import java.util.UUID

class PeopleIdentity(val subject: String, val authorization: AuthorizationLease, val mailbox: String, val features: Set<String>) {
    fun same(other: PeopleIdentity?) = other != null && subject == other.subject && authorization === other.authorization && mailbox == other.mailbox && features == other.features
    override fun toString() = "PeopleIdentity(redacted)"
}
/** The address book stays in memory. Only `hash` is used by discovery requests. */
class DeviceContact(val phone: String, val name: String) {
    val hash = PeopleInput.hash(phone)
    override fun toString() = "DeviceContact(redacted)"
}
class InviteReview internal constructor(val context: String, val contact: DeviceContact) {
    override fun toString() = "InviteReview(redacted)"
}
interface LocalAliases {
    fun read(subject: String, mailbox: String): Map<String, String>
    fun write(subject: String, mailbox: String, values: Map<String, String>)
    fun clear()
}
data class PeopleState(
    val context: String = UUID.randomUUID().toString(), val mailbox: String? = null, val features: Set<String> = emptySet(),
    val busy: Boolean = false, val message: String? = null, val phones: List<VerifiedPhone> = emptyList(),
    val challenge: PhoneChallenge? = null, val ownName: ProfileName? = null, val nameLoaded: Boolean = false,
    val imported: List<DeviceContact> = emptyList(), val checked: Set<String> = emptySet(), val matches: Map<String, DiscoveryMatch> = emptyMap(),
    val remaining: Int? = null, val invite: InviteReview? = null, val invited: Set<String> = emptySet(),
    val aliases: Map<String, String> = emptyMap(), val profiles: Map<String, PeerProfile?> = emptyMap(),
) {
    fun label(peer: String, legacyAlias: String? = null, fallback: String = displayName(peer)): String {
        val profile = profiles[peer]
        return (aliases[profile?.address] ?: aliases[peer])?.takeIf { it.isNotBlank() } ?: legacyAlias?.takeIf { it.isNotBlank() }
            ?: profile?.name?.let { "~$it" } ?: fallback
    }
    override fun toString() = "PeopleState(redacted)"
}

/** One human authorization and one inbox own every dialog, import, result and SMS confirmation. */
class PeopleStore(private val api: PeopleApi, private val scope: CoroutineScope, private val identity: () -> PeopleIdentity?,
    private val aliases: LocalAliases, private val now: () -> Long = { System.currentTimeMillis() / 1000 },
    private val expired: (AuthorizationLease) -> Unit = {}) {
    private val mutable = MutableStateFlow(PeopleState())
    val state = mutable.asStateFlow()
    private var bound: PeopleIdentity? = null
    private var actor: AuthorizationLease? = null
    private var job: Job? = null
    private var namesJob: Job? = null
    private var requestedProfiles = emptyMap<String, Long>()
    private var importedPhone: String? = null
    fun isCurrent(context: String) = context == state.value.context && actor?.isValid == true && bound?.same(identity()) == true
    fun synchronize() {
        val next = identity()?.takeIf { it.subject.isNotBlank() && PeopleInput.mailbox(it.mailbox) && it.authorization.isValid }
        if (bound?.same(next) == true && actor?.isValid == true || next == null && bound == null) return
        reset()
        if (next != null) {
            val child = try { next.authorization.child() } catch (_: AuthorizationRetired) { return }
            bound = next; actor = child
            val local = aliases.read(next.subject, next.mailbox).filter { (key, value) -> validAddress(key) && runCatching { PeopleInput.name(value, alias = true) }.getOrNull() == value }
            mutable.value = PeopleState(context = child.id, mailbox = next.mailbox, features = next.features, aliases = local)
        }
    }
    fun reset(clearAliases: Boolean = false) {
        actor?.retire(); job?.cancel(); namesJob?.cancel(); actor = null; bound = null; job = null; namesJob = null
        importedPhone = null; requestedProfiles = emptyMap(); mutable.value = PeopleState()
        if (clearAliases) aliases.clear()
    }
    private fun start(context: String, flag: String, work: suspend (String, String) -> Unit): Job? {
        synchronize()
        val source = bound ?: return null
        val owner = actor ?: return null
        if (!isCurrent(context) || flag !in state.value.features || state.value.busy) return null
        val request = owner.child()
        mutable.update { it.copy(busy = true, message = null) }
        val task = scope.launch(AuthorizationContext(request), start = CoroutineStart.LAZY) {
            try { request.requireActive(); work(context, source.mailbox) }
            catch (cancelled: CancellationException) { throw cancelled }
            catch (failure: Exception) {
                if (isCurrent(context)) {
                    if (failure is SessionExpired) expired(source.authorization)
                    mutable.update { it.copy(message = error(failure)) }
                }
            } finally {
                request.retire()
                if (isCurrent(context)) { mutable.update { it.copy(busy = false) }; job = null }
            }
        }
        job = task; task.start(); return task
    }
    fun loadPhones(context: String) = start(context, Features.PHONE_NUMBERS) { c, mailbox ->
        val result = api.phones(mailbox); if (isCurrent(c)) mutable.update { it.copy(phones = result) }
    }
    fun sendCode(context: String, phone: String): Job? {
        if (!PeopleInput.phone(phone)) return null
        if (state.value.challenge?.resendAfter?.let { it > now() } == true && importedPhone == phone) return null
        return start(context, Features.PHONE_NUMBERS) { c, mailbox ->
            val result = api.sendCode(mailbox, phone)
            if (isCurrent(c)) { importedPhone = phone; mutable.update { it.copy(challenge = result, message = "Code sent. Check your text messages.") } }
        }
    }
    fun verify(context: String, code: String, discoverable: Boolean): Job? {
        val challenge = state.value.challenge ?: return null
        if (!code.matches(Regex("[0-9]{6}")) || challenge.expiresAt <= now()) return null
        return start(context, Features.PHONE_NUMBERS) { c, mailbox ->
            val result = api.verify(mailbox, challenge.id, code, discoverable)
            if (isCurrent(c)) {
                importedPhone = null
                mutable.update { it.copy(challenge = null, phones = it.phones.filter { n -> n.id != result.id && n.identity != result.identity } + result,
                    message = "Phone number verified.") }
            }
        }
    }
    fun discoverable(context: String, id: String, value: Boolean): Job? {
        if (state.value.phones.none { it.id == id }) return null
        return start(context, Features.PHONE_NUMBERS) { c, mailbox ->
            val result = api.discoverable(mailbox, id, value)
            if (isCurrent(c)) mutable.update { it.copy(phones = it.phones.map { n -> if (n.id == id) result else n }, message = "Discoverability saved.") }
        }
    }
    fun unlink(context: String, id: String): Job? {
        if (state.value.phones.none { it.id == id }) return null
        return start(context, Features.PHONE_NUMBERS) { c, mailbox ->
            api.unlink(mailbox, id)
            if (isCurrent(c)) mutable.update { it.copy(phones = it.phones.filter { n -> n.id != id }, message = "Phone number unlinked.") }
        }
    }
    /** Called only by the explicit import action, after the Android permission/disclosure. */
    fun discover(context: String, contacts: List<DeviceContact>): Job? = start(context, Features.CONTACT_DISCOVERY) { c, mailbox ->
        val unique = contacts.distinctBy { it.hash }.take(5000)
        mutable.update { it.copy(imported = unique, checked = emptySet(), matches = emptyMap(), invite = null, remaining = null) }
        var offset = 0
        var remaining = 200 // Safe first batch for a new account; follow the server's allowance next.
        while (offset < unique.size && remaining > 0 && isCurrent(c)) {
            val hashes = unique.drop(offset).take(minOf(200, remaining)).map { it.hash }
            val result = api.discover(mailbox, hashes)
            if (!isCurrent(c)) return@start
            remaining = result.remaining; offset += hashes.size
            mutable.update { it.copy(checked = it.checked + hashes, matches = it.matches + result.matches.associateBy { m -> m.hash }, remaining = remaining) }
        }
        if (isCurrent(c)) mutable.update { it.copy(message = if (offset < unique.size) "Checked $offset of ${unique.size} numbers. The daily allowance is used; try the rest tomorrow." else "Checked $offset numbers. Only people who allow discovery appear.") }
    }
    fun clearImport(context: String) {
        if (isCurrent(context) && !state.value.busy) mutable.update { it.copy(imported = emptyList(), checked = emptySet(), matches = emptyMap(), invite = null, invited = emptySet()) }
    }
    fun reviewInvite(context: String, hash: String): InviteReview? {
        synchronize()
        val s = state.value
        if (!isCurrent(context) || s.busy || Features.SMS_INVITES !in s.features || hash !in s.checked || hash in s.matches || hash in s.invited) return null
        val contact = s.imported.singleOrNull { it.hash == hash } ?: return null
        return InviteReview(context, contact).also { review -> mutable.update { it.copy(invite = review) } }
    }
    fun cancelInvite() { mutable.update { it.copy(invite = null) } }
    fun invite(review: InviteReview): Job? {
        if (!isCurrent(review.context) || state.value.invite !== review) return null
        return start(review.context, Features.SMS_INVITES) { c, mailbox ->
            mutable.update { it.copy(invite = null) } // Each request consumes its separate confirmation, even on timeout.
            val result = api.invite(mailbox, review.contact.phone)
            if (isCurrent(c)) mutable.update { it.copy(invited = it.invited + review.contact.hash, message =
                if (result.status == "sent") "Invitation sent." else "An invitation was not sent. This number may have been invited already or may not accept invitations.") }
        }
    }
    fun loadName(context: String) = start(context, Features.DISPLAY_NAMES) { c, mailbox ->
        val result = api.ownName(mailbox); if (isCurrent(c)) mutable.update { it.copy(ownName = result, nameLoaded = true) }
    }
    fun saveName(context: String, name: String, visibility: String) = start(context, Features.DISPLAY_NAMES) { c, mailbox ->
        val result = api.saveName(mailbox, name, visibility)
        if (isCurrent(c)) mutable.update { it.copy(ownName = result, nameLoaded = true, message = "Display name saved.") }
    }
    fun removeName(context: String) = start(context, Features.DISPLAY_NAMES) { c, mailbox ->
        api.removeName(mailbox); if (isCurrent(c)) mutable.update { it.copy(ownName = null, nameLoaded = true, message = "Display name removed.") }
    }
    fun alias(context: String, peer: String, value: String): Boolean {
        synchronize(); val owner = bound ?: return false
        if (!isCurrent(context) || !validAddress(peer)) return false
        val normalized = if (value.isBlank()) null else runCatching { PeopleInput.name(value, alias = true) }.getOrNull() ?: return false
        actor!!.withActive {
            val next = state.value.aliases.toMutableMap().apply { if (normalized == null) remove(peer) else put(peer, normalized) }
            aliases.write(owner.subject, owner.mailbox, next); mutable.update { it.copy(aliases = next) }
        }
        return true
    }
    fun loadProfiles(context: String, addresses: List<String>) {
        synchronize(); val owner = actor ?: return
        val source = bound ?: return
        if (!isCurrent(context) || Features.DISPLAY_NAMES !in state.value.features || namesJob?.isActive == true) return
        val wanted = addresses.filter { validAddress(it) }.distinct().filter { requestedProfiles[it]?.let { time -> now() - time < 60 } != true }.take(100)
        if (wanted.isEmpty()) return
        val request = owner.child()
        namesJob = scope.launch(AuthorizationContext(request)) {
            try {
                val result = api.profiles(source.mailbox, wanted)
                if (isCurrent(context)) {
                    requestedProfiles = requestedProfiles + wanted.associateWith { now() }
                    mutable.update { it.copy(profiles = it.profiles + wanted.associateWith { key -> result[key] }) }
                }
            } catch (cancelled: CancellationException) { throw cancelled }
            catch (_: Exception) { /* Keep addresses visible; profile failure never blocks messaging. */ }
            finally { request.retire() }
        }
    }
    companion object {
        fun error(failure: Exception): String = when ((failure as? ApiException)?.code) {
            "invalid_code" -> "That code did not match. Check the latest text message."
            "challenge_expired" -> "That code has expired. Request another code."
            "phone_required" -> "Verify your own phone number before sending invitations."
            "phone_not_supported" -> "This number cannot receive Pigeonpost verification messages."
            "rate_limited", "discovery_locked" -> "The limit has been reached. Please try again later."
            "sms_unavailable" -> "Text messages are temporarily unavailable. Please try again later."
            "feature_disabled" -> "This feature is not available for this inbox yet."
            else -> if (failure is IllegalArgumentException) "Check the details and try again." else "The request could not be confirmed. Check its status before trying again."
        }
    }
}
