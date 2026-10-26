package dev.pigeonpost.core

import kotlinx.serialization.json.*
import java.io.IOException
import java.security.MessageDigest
import java.time.Instant
import java.util.Base64

enum class StrongSignInProblem { UNVERIFIED, DIFFERENT_ACCOUNT }
class StrongSignInException(val problem: StrongSignInProblem = StrongSignInProblem.UNVERIFIED) : IOException(
    if (problem == StrongSignInProblem.DIFFERENT_ACCOUNT) "Use the account already signed in to Pigeonpost."
    else "The fresh sign-in could not be verified. Try again."
)

/**
 * Checks the result of the app's fixed HTTPS token exchange, never uploaded or pasted tokens.
 * The exchange authenticates its source; this is not a stand-alone JWT signature verifier.
 * The postbox independently verifies its bearer and the live permission before any decision.
 */
object StrongSignInProof {
    private data class Token(val header: JsonObject, val claims: JsonObject)
    private fun parsed(raw: String?): Token {
        if (raw == null || raw.length > 32768) throw StrongSignInException()
        val parts = raw.split('.')
        if (parts.size != 3 || parts.any { !Regex("[A-Za-z0-9_-]+").matches(it) }) throw StrongSignInException()
        return try {
            fun objectAt(index: Int) = WireJson.parseToJsonElement(Base64.getUrlDecoder().decode(parts[index]).toString(Charsets.UTF_8)) as? JsonObject ?: throw StrongSignInException()
            Token(objectAt(0), objectAt(1))
        } catch (failure: StrongSignInException) { throw failure }
        catch (_: Exception) { throw StrongSignInException() }
    }
    private fun JsonObject.string(key: String): String = (this[key] as? JsonPrimitive)?.takeIf(JsonPrimitive::isString)?.content ?: throw StrongSignInException()
    private fun JsonObject.number(key: String): Double = (this[key] as? JsonPrimitive)?.takeUnless(JsonPrimitive::isString)?.doubleOrNull?.takeIf(Double::isFinite) ?: throw StrongSignInException()

    /** Only identifies an already accepted, protected token from that same trusted exchange. */
    fun subject(raw: String?, issuer: String): String? = try {
        val claims = parsed(raw).claims
        if (claims.string("iss") != issuer) null else claims.string("sub").takeIf { it.isNotEmpty() && it.toByteArray(Charsets.UTF_8).size <= 256 && it.none(Character::isISOControl) }
    } catch (_: StrongSignInException) { null }

    fun validate(idToken: String?, accessToken: String, subject: String, nonce: String, requirement: StrongSignInRequirement,
                 issuer: String, client: String, startedAt: Long, now: Long = Instant.now().epochSecond) {
        if (subject.isEmpty() || subject.toByteArray(Charsets.UTF_8).size > 256 || nonce.isEmpty() || nonce.length > 128 ||
            startedAt !in 0..253_402_300_799L || now !in 0..253_402_300_799L || startedAt > now + 30) throw StrongSignInException()
        val id = parsed(idToken); val access = parsed(accessToken)
        if (id.claims.string("sub") != subject || access.claims.string("sub") != subject)
            throw StrongSignInException(StrongSignInProblem.DIFFERENT_ACCOUNT)
        val audiences = when (val value = id.claims["aud"]) {
            is JsonPrimitive -> if (value.isString) listOf(value.content) else emptyList()
            is JsonArray -> value.map { (it as? JsonPrimitive)?.takeIf(JsonPrimitive::isString)?.content ?: throw StrongSignInException() }
            else -> emptyList()
        }
        if (id.header.string("alg") != "RS256" || access.header.string("alg") != "RS256" ||
            id.claims.string("iss") != issuer || access.claims.string("iss") != issuer || audiences != listOf(client) ||
            id.claims["azp"]?.let { it !is JsonPrimitive || !it.isString || it.content != client } == true ||
            access.claims.string("azp") != client || id.claims.string("nonce") != nonce) throw StrongSignInException()
        for (token in listOf(id, access)) {
            val expiration = token.claims.number("exp"); val issued = token.claims.number("iat"); val authenticated = token.claims.number("auth_time")
            if (expiration <= now || expiration <= issued || issued < startedAt - 30 || issued > now + 30 ||
                authenticated > now + 30 || authenticated < now - requirement.maxAge - 30 ||
                requirement.maxAge == 0 && authenticated < startedAt - 5 || token.claims.string("acr") !in requirement.acrValues) throw StrongSignInException()
        }
        id.claims["at_hash"]?.let {
            val hash = (it as? JsonPrimitive)?.takeIf(JsonPrimitive::isString)?.content ?: throw StrongSignInException()
            val digest = MessageDigest.getInstance("SHA-256").digest(accessToken.toByteArray(Charsets.UTF_8)).copyOfRange(0, 16)
            val expected = Base64.getUrlEncoder().withoutPadding().encodeToString(digest)
            if (!MessageDigest.isEqual(hash.toByteArray(Charsets.UTF_8), expected.toByteArray(Charsets.UTF_8))) throw StrongSignInException()
        }
    }
}
