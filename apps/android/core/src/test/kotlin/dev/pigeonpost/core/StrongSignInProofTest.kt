package dev.pigeonpost.core

import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test
import java.util.Base64

/** Synthetic token-endpoint responses, not signed credentials or a public JWT verification API. */
class StrongSignInProofTest {
    private val now = 1_800_000_000L
    private val issuer = "https://issuer.invalid/realms/test"
    private val client = "pigeonpost-mobile"
    private val requirement = StrongSignInRequirement.create(listOf("2"), 600)!!
    private val access = WireJson.parseToJsonElement("""{"iss":"https://issuer.invalid/realms/test","sub":"member","azp":"pigeonpost-mobile","exp":1800003600,"iat":1800000000,"auth_time":1800000000,"acr":"2"}""").jsonObject
    private val id = JsonObject(access + mapOf("aud" to JsonPrimitive(client), "nonce" to JsonPrimitive("request-nonce")))
    private fun jwt(claims: JsonObject, algorithm: String = "RS256"): String {
        fun encoded(text: String) = Base64.getUrlEncoder().withoutPadding().encodeToString(text.toByteArray(Charsets.UTF_8))
        return encoded("""{"alg":"$algorithm"}""") + "." + encoded(claims.toString()) + ".c2ln"
    }
    private fun changed(base: JsonObject, vararg fields: Pair<String, JsonElement>) = JsonObject(base + fields.toMap())
    private fun verify(idToken: String? = jwt(id), accessToken: String = jwt(access), needs: StrongSignInRequirement = requirement,
                       subject: String = "member", nonce: String = "request-nonce", began: Long = now, instant: Long = now) =
        StrongSignInProof.validate(idToken, accessToken, subject, nonce, needs, issuer, client, began, instant)
    private fun refuses(idClaims: JsonObject = id, accessClaims: JsonObject = access, problem: StrongSignInProblem = StrongSignInProblem.UNVERIFIED) {
        val failure = runCatching { verify(jwt(idClaims), jwt(accessClaims)) }.exceptionOrNull()
        assertTrue(failure is StrongSignInException); assertEquals(problem, (failure as StrongSignInException).problem)
    }
    @Test fun aFreshSamePersonResponseMeetsTheRequestedMethod() {
        verify()
        verify(jwt(changed(id, "aud" to JsonArray(listOf(JsonPrimitive(client))))))
        verify(jwt(JsonObject(id - "azp")))
    }
    @Test fun bothTokensMustBelongToTheAlreadyAcceptedPerson() {
        refuses(changed(id, "sub" to JsonPrimitive("different")), problem = StrongSignInProblem.DIFFERENT_ACCOUNT)
        refuses(accessClaims = changed(access, "sub" to JsonPrimitive("different")), problem = StrongSignInProblem.DIFFERENT_ACCOUNT)
        val failure = runCatching { verify(subject = "private-account-subject") }.exceptionOrNull()!!
        assertFalse(failure.toString().contains("private-account-subject")); assertFalse(failure.toString().contains(jwt(id)))
    }
    @Test fun nonceIssuerAudienceAndAuthorizedPartyAreBoundToTheRequest() {
        for (claims in listOf(changed(id, "nonce" to JsonPrimitive("different")), changed(id, "iss" to JsonPrimitive("https://untrusted.invalid")),
            changed(id, "aud" to JsonPrimitive("other-client")), changed(id, "aud" to JsonArray(listOf(JsonPrimitive(client), JsonPrimitive("other-client")))),
            changed(id, "azp" to JsonPrimitive("other-client")), JsonObject(id - "nonce"))) refuses(claims)
        refuses(accessClaims = changed(access, "iss" to JsonPrimitive("https://untrusted.invalid")))
        refuses(accessClaims = changed(access, "azp" to JsonPrimitive("other-client")))
    }
    @Test fun staleExpiredAndFutureAuthenticationCannotPassAsFresh() {
        for (values in listOf(changed(id, "exp" to JsonPrimitive(now)), changed(id, "exp" to JsonPrimitive(now - 10)),
            changed(id, "iat" to JsonPrimitive(now - 31)), changed(id, "iat" to JsonPrimitive(now + 31)),
            changed(id, "auth_time" to JsonPrimitive(now - 631)), changed(id, "auth_time" to JsonPrimitive(now + 31)),
            changed(id, "auth_time" to JsonPrimitive("$now")))) refuses(values)
        refuses(accessClaims = changed(access, "exp" to JsonPrimitive(now)))
        refuses(accessClaims = changed(access, "auth_time" to JsonPrimitive(now - 631)))
        assertTrue(runCatching { verify(began = now + 31) }.exceptionOrNull() is StrongSignInException)
        verify(jwt(changed(id, "iat" to JsonPrimitive(now - 30), "auth_time" to JsonPrimitive(now - 630))))
    }
    @Test fun maxAgeZeroRequiresAuthenticationDuringThisInteraction() {
        val fresh = StrongSignInRequirement.create(listOf("2"), 0)!!
        verify(needs = fresh)
        assertTrue(runCatching { verify(jwt(changed(id, "auth_time" to JsonPrimitive(now - 6))), needs = fresh) }.exceptionOrNull() is StrongSignInException)
        assertTrue(runCatching { verify(accessToken = jwt(changed(access, "auth_time" to JsonPrimitive(now - 6))), needs = fresh) }.exceptionOrNull() is StrongSignInException)
    }
    @Test fun theBearerAsWellAsTheIdTokenMustCarryTheRequiredMethod() {
        refuses(changed(id, "acr" to JsonPrimitive("1")))
        refuses(accessClaims = changed(access, "acr" to JsonPrimitive("1")))
        refuses(accessClaims = JsonObject(access - "auth_time"))
        refuses(accessClaims = changed(access, "acr" to JsonPrimitive(2)))
    }
    @Test fun aPublishedAccessHashBindsTheExactReturnedBearerBytes() {
        // Fixed SHA-256 half-hash of this synthetic access token; not a generated expectation.
        val bound = jwt(changed(id, "at_hash" to JsonPrimitive("P5NtcgOZLcHiopC2EJDOBg")))
        verify(bound)
        assertTrue(runCatching { verify(bound, jwt(changed(access, "extra" to JsonPrimitive("changed")))) }.exceptionOrNull() is StrongSignInException)
        refuses(changed(id, "at_hash" to JsonPrimitive("wrong")))
        refuses(changed(id, "at_hash" to JsonPrimitive(123)))
    }
    @Test fun algorithmsSyntaxSizeAndMissingClaimsFailWithoutLeakingCandidates() {
        for (algorithm in listOf("none", "HS256", "")) {
            assertTrue(runCatching { verify(jwt(id, algorithm)) }.exceptionOrNull() is StrongSignInException)
            assertTrue(runCatching { verify(accessToken = jwt(access, algorithm)) }.exceptionOrNull() is StrongSignInException)
        }
        for (token in listOf(null, "private-token", "a.b.c.d", "a..c", "a.b.c=", "a".repeat(32769), "!!!!.aaaa.sig")) {
            val failure = runCatching { verify(token) }.exceptionOrNull()
            assertTrue(failure is StrongSignInException); assertFalse(failure.toString().contains("private-token"))
        }
        refuses(JsonObject(id - "exp")); refuses(changed(id, "exp" to WireJson.parseToJsonElement("1e309")))
        assertTrue(runCatching { verify(nonce = "") }.exceptionOrNull() is StrongSignInException)
        assertTrue(runCatching { verify(nonce = "x".repeat(129)) }.exceptionOrNull() is StrongSignInException)
    }
    @Test fun onlyTheProtectedTrustedIssuerCanSupplyTheExpectedSubject() {
        assertEquals("member", StrongSignInProof.subject(jwt(access), issuer))
        assertNull(StrongSignInProof.subject(jwt(access), "https://untrusted.invalid"))
        assertNull(StrongSignInProof.subject("private-malformed", issuer))
        assertNull(StrongSignInProof.subject(jwt(changed(access, "sub" to JsonPrimitive(""))), issuer))
        assertNull(StrongSignInProof.subject(jwt(changed(access, "sub" to JsonPrimitive("x".repeat(257)))), issuer))
    }
}
