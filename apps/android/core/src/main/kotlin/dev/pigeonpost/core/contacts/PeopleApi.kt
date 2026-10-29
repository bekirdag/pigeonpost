package dev.pigeonpost.core.contacts

import dev.pigeonpost.core.*
import dev.pigeonpost.core.groups.GroupTransport
import kotlinx.coroutines.currentCoroutineContext
import kotlinx.serialization.SerialName
import kotlinx.serialization.Serializable
import kotlinx.serialization.json.*
import java.net.URLEncoder
import java.security.MessageDigest
import java.text.Normalizer

object PeopleInput {
    fun phone(value: String) = value.matches(Regex("\\+[1-9][0-9]{7,14}"))
    fun id(value: String) = value.matches(Regex("[A-Za-z0-9_-]{1,128}"))
    fun mailbox(value: String) = value.startsWith("/k/") && validAddress(value)
    fun hash(value: String): String {
        require(phone(value)) { "Use a phone number with its country code." }
        return MessageDigest.getInstance("SHA-256").digest(("pigeonpost:phone:v1:" + value).toByteArray(Charsets.UTF_8))
            .take(16).joinToString("") { "%02x".format(it.toInt() and 255) }
    }
    fun name(value: String, alias: Boolean = false): String {
        require(value.codePoints().noneMatch { c -> c == '/'.code || c == '@'.code ||
            Character.getType(c) in setOf(Character.CONTROL.toInt(), Character.FORMAT.toInt(), Character.SURROGATE.toInt(), Character.LINE_SEPARATOR.toInt(), Character.PARAGRAPH_SEPARATOR.toInt()) ||
            c in setOf(0x034f, 0x115f, 0x1160, 0x17b4, 0x17b5, 0x2800, 0x3164, 0xffa0) ||
            c in 0x180b..0x180f || c in 0xfe00..0xfe0f || c in 0xfff0..0xfffb || c in 0xe0000..0xe007f }) { "Use a name without addresses or invisible characters." }
        val normalized = Normalizer.normalize(value.trim().replace(Regex("[\\p{Zs} ]+"), " "), Normalizer.Form.NFC)
        require(normalized.codePointCount(0, normalized.length) in 1..if (alias) 64 else 40) { "Use ${if (alias) "1–64" else "1–40"} characters." }
        val squashed = normalized.lowercase(java.util.Locale.ROOT).filter(Char::isLetterOrDigit)
        require(squashed.isNotEmpty()) { "Include at least one letter or digit." }
        require(alias || (!squashed.contains("pigeonpost") && squashed !in
            setOf("admin", "support", "official", "verified", "security", "system", "moderator"))) { "Choose a personal name." }
        return normalized
    }
    val visibility = setOf("known", "everyone", "nobody")
}

@Serializable data class VerifiedPhone(val id: String, val identity: String, @SerialName("country_code") val countryCode: String,
    val masked: String, val discoverable: Boolean, @SerialName("verified_at") val verifiedAt: Long) {
    override fun toString() = "VerifiedPhone(redacted)"
}
@Serializable data class PhoneChallenge(@SerialName("challenge_id") val id: String, @SerialName("expires_at") val expiresAt: Long,
    @SerialName("resend_after") val resendAfter: Long, @SerialName("code_length") val codeLength: Int)
@Serializable data class ProfileName(@SerialName("display_name") val name: String, val visibility: String, @SerialName("updated_at") val updatedAt: Long)
@Serializable data class PeerProfile(val address: String, val handle: String? = null, @SerialName("display_name") val name: String? = null)
@Serializable data class DiscoveryMatch(val hash: String, val address: String, val handle: String? = null, @SerialName("display_name") val name: String? = null)
@Serializable data class DiscoveryResult(val matches: List<DiscoveryMatch>, val checked: Int, @SerialName("new") val fresh: Int,
    @SerialName("remaining_today") val remaining: Int)
@Serializable data class InvitationResult(val status: String, val reason: String? = null, @SerialName("invite_id") val id: String? = null)
@Serializable private data class PhonesResponse(val numbers: List<VerifiedPhone>, @SerialName("max_numbers") val max: Int)
@Serializable private data class PhoneResponse(val number: VerifiedPhone)
@Serializable private data class NameResponse(val name: ProfileName?)
@Serializable private data class ProfilesResponse(val profiles: Map<String, PeerProfile?>)

interface PeopleApi {
    suspend fun phones(mailbox: String): List<VerifiedPhone>
    suspend fun sendCode(mailbox: String, phone: String): PhoneChallenge
    suspend fun verify(mailbox: String, challenge: String, code: String, discoverable: Boolean): VerifiedPhone
    suspend fun discoverable(mailbox: String, id: String, value: Boolean): VerifiedPhone
    suspend fun unlink(mailbox: String, id: String)
    suspend fun discover(mailbox: String, hashes: List<String>): DiscoveryResult
    suspend fun invite(mailbox: String, phone: String): InvitationResult
    suspend fun ownName(mailbox: String): ProfileName?
    suspend fun saveName(mailbox: String, name: String, visibility: String): ProfileName
    suspend fun removeName(mailbox: String)
    suspend fun profiles(mailbox: String, addresses: List<String>): Map<String, PeerProfile?>
}

/** Uses the same immutable authorization context as groups; no background contacts or SMS retries. */
class PeopleClient(private val transport: GroupTransport) : PeopleApi {
    private fun bad(): Nothing = throw ApiException(200, "bad_response")
    private suspend fun json(mailbox: String, method: String, path: String, body: JsonObject? = null): JsonElement {
        require(PeopleInput.mailbox(mailbox))
        val owner = currentCoroutineContext()[AuthorizationContext]?.lease ?: throw AuthorizationRetired()
        owner.requireActive()
        val raw = transport.groupJson(mailbox, method, path, body)
        owner.requireActive()
        if (raw.length > 512 * 1024 || raw.toByteArray(Charsets.UTF_8).size > 512 * 1024) bad()
        return try { WireJson.parseToJsonElement(raw) } catch (_: IllegalArgumentException) { bad() }
    }
    private inline fun <reified T> decode(value: JsonElement): T = try { WireJson.decodeFromJsonElement(value) } catch (_: IllegalArgumentException) { bad() }
    private fun phone(value: VerifiedPhone): VerifiedPhone = value.also {
        if (!PeopleInput.id(it.id) || !PeopleInput.mailbox(it.identity) || !it.countryCode.matches(Regex("[0-9]{1,3}")) ||
            it.masked.length !in 1..64 || it.masked.any(Char::isISOControl) || it.verifiedAt <= 0) bad()
    }
    private fun name(value: ProfileName): ProfileName = value.also {
        if (it.visibility !in PeopleInput.visibility || it.updatedAt <= 0 || runCatching { PeopleInput.name(it.name) }.getOrNull() != it.name) bad()
    }
    private fun peer(address: String, handle: String?, name: String?) {
        if (!PeopleInput.mailbox(address) || handle?.let { !validAddress(it) || it.startsWith("/k/") } == true ||
            name?.let { runCatching { PeopleInput.name(it) }.getOrNull() != it } == true) bad()
    }
    override suspend fun phones(mailbox: String): List<VerifiedPhone> = decode<PhonesResponse>(json(mailbox, "GET", "/v1/phone")).let {
        if (it.max !in 1..3 || it.numbers.size > it.max || it.numbers.map { n -> n.id }.distinct().size != it.numbers.size) bad()
        it.numbers.map(::phone)
    }
    override suspend fun sendCode(mailbox: String, phone: String): PhoneChallenge {
        require(PeopleInput.phone(phone))
        return decode<PhoneChallenge>(json(mailbox, "POST", "/v1/phone/verify", buildJsonObject { put("phone", phone); put("identity", mailbox) })).also {
            if (!PeopleInput.id(it.id) || it.expiresAt <= 0 || it.resendAfter < 0 || it.codeLength != 6) bad()
        }
    }
    override suspend fun verify(mailbox: String, challenge: String, code: String, discoverable: Boolean): VerifiedPhone {
        require(PeopleInput.id(challenge) && code.matches(Regex("[0-9]{6}")))
        return phone(decode<PhoneResponse>(json(mailbox, "POST", "/v1/phone/verify/$challenge", buildJsonObject {
            put("code", code); put("discoverable", discoverable)
        })).number).also { if (it.identity != mailbox) bad() }
    }
    override suspend fun discoverable(mailbox: String, id: String, value: Boolean): VerifiedPhone {
        require(PeopleInput.id(id))
        return phone(decode<PhoneResponse>(json(mailbox, "PATCH", "/v1/phone/$id", buildJsonObject { put("discoverable", value) })).number)
            .also { if (it.id != id || it.discoverable != value) bad() }
    }
    override suspend fun unlink(mailbox: String, id: String) { require(PeopleInput.id(id)); json(mailbox, "DELETE", "/v1/phone/$id") }
    override suspend fun discover(mailbox: String, hashes: List<String>): DiscoveryResult {
        require(hashes.size in 1..200 && hashes.distinct().size == hashes.size && hashes.all { it.matches(Regex("[a-f0-9]{32}")) })
        return decode<DiscoveryResult>(json(mailbox, "POST", "/v1/contacts/discover", buildJsonObject {
            put("hashes", JsonArray(hashes.map(::JsonPrimitive)))
        })).also { r ->
            if (r.checked != hashes.size || r.fresh !in 0..r.checked || r.remaining !in 0..1000 || r.matches.size > hashes.size ||
                r.matches.map { it.hash }.distinct().size != r.matches.size) bad()
            r.matches.forEach { if (it.hash !in hashes) bad(); peer(it.address, it.handle, it.name) }
        }
    }
    override suspend fun invite(mailbox: String, phone: String): InvitationResult {
        require(PeopleInput.phone(phone))
        return decode<InvitationResult>(json(mailbox, "POST", "/v1/contacts/invites", buildJsonObject { put("phone", phone); put("confirm", true) })).also {
            if (!(it.status == "sent" && it.id?.let(PeopleInput::id) == true || it.status == "not_sent" && it.reason in setOf("already_invited", "unavailable"))) bad()
        }
    }
    override suspend fun ownName(mailbox: String) = decode<NameResponse>(json(mailbox, "GET", "/v1/profile/name")).name?.let(::name)
    override suspend fun saveName(mailbox: String, name: String, visibility: String): ProfileName {
        val normalized = PeopleInput.name(name); require(visibility in PeopleInput.visibility)
        return decode<NameResponse>(json(mailbox, "PUT", "/v1/profile/name", buildJsonObject { put("display_name", normalized); put("visibility", visibility) })).name?.let(::name) ?: bad()
    }
    override suspend fun removeName(mailbox: String) { json(mailbox, "DELETE", "/v1/profile/name") }
    override suspend fun profiles(mailbox: String, addresses: List<String>): Map<String, PeerProfile?> {
        require(addresses.size in 1..100 && addresses.all { validAddress(it) })
        return decode<ProfilesResponse>(json(mailbox, "GET", "/v1/profiles?addresses=" + URLEncoder.encode(addresses.joinToString(","), "UTF-8"))).profiles.also { entries ->
            if (entries.keys.any { it !in addresses }) bad()
            entries.forEach { (key, value) -> value?.let {
                peer(it.address, it.handle, it.name)
                if (key != it.address && key != it.handle) bad()
            } }
        }
    }
}
