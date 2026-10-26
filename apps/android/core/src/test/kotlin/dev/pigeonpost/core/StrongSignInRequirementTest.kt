package dev.pigeonpost.core

import kotlinx.serialization.json.*
import org.junit.Assert.*
import org.junit.Test

class StrongSignInRequirementTest {
    private fun problem(text: String) = WireJson.parseToJsonElement(text).jsonObject
    @Test fun methodsAndRecencyAreStrictlyBounded() {
        assertNotNull(StrongSignInRequirement.create(listOf("2", "urn:example:mfa"), 600))
        assertNotNull(StrongSignInRequirement.create(listOf("2"), 0))
        for (values in listOf(emptyList(), listOf("2", "2"), listOf("İ"), listOf("two factors"), listOf("2\n"), listOf("2?redirect=evil"), listOf("a".repeat(129)), (1..9).map(Int::toString)))
            assertNull(StrongSignInRequirement.create(values, 600))
        for (age in listOf(-1, 601, Int.MAX_VALUE)) assertNull(StrongSignInRequirement.create(listOf("2"), age))
    }
    @Test fun headerRequirementsAreParsedWithoutAdoptingAnotherIssuer() {
        val requirement = StrongSignInRequirement.from(null, "Bearer error=\"insufficient_user_authentication\", acr_values=\"2 urn:example:mfa\", max_age=\"600\", authorization_uri=\"https://untrusted.invalid\"")!!
        assertEquals(listOf("2", "urn:example:mfa"), requirement.acrValues)
        assertEquals(600, requirement.maxAge)
    }
    @Test fun bodySupportsTheTwoPublishedAcrShapesButNotCoercedNumbers() {
        for (wire in listOf("""{"acr_values":["2"],"max_age":600}""", """{"acr_values":"2","max_age":600}""")) {
            assertEquals("2", StrongSignInRequirement.from(problem(wire), null)!!.requestedACR)
        }
        for (wire in listOf("""{"acr_values":[2],"max_age":600}""", """{"acr_values":["2"],"max_age":"600"}""", """{"acr_values":["2"],"max_age":true}""", """{"acr_values":["2"],"max_age":600.5}"""))
            assertNull(StrongSignInRequirement.from(problem(wire), null))
    }
    @Test fun descriptionsCannotInjectParametersAndQuotedCommasStayInsideTheirValue() {
        val forged = "Bearer error=\"insufficient_user_authentication\", error_description=\"example, acr_values=2, max_age=600\""
        assertNull(StrongSignInRequirement.from(null, forged))
        val valid = forged + ", acr_values=\"2\", max_age=600"
        assertEquals("2", StrongSignInRequirement.from(null, valid)!!.requestedACR)
    }
    @Test fun malformedAndDuplicateHeadersDoNotInventARequirement() {
        val prefix = "Bearer error=insufficient_user_authentication, "
        for (fields in listOf("acr_values=2, acr_values=1, max_age=600", "acr_values=2, max_age=600, max_age=1", "acr_values=\"2, max_age=600", "acr_values=2, max_age=+600", "acr_values=2, max_age=601", "acr_values=2, max_age=600,", "acr_values=2, max_age=600\n")) {
            assertNull(fields, StrongSignInRequirement.from(null, prefix + fields))
            assertTrue(ApiException(401, null, authenticationChallenge = prefix + fields).needsStrongSignIn)
        }
        assertNull(StrongSignInRequirement.from(null, prefix + "x".repeat(2049)))
    }
    @Test fun aTrustedBodyCanSupplyRequirementsWhenTheHeaderCannot() {
        val body = problem("""{"error":"step_up_required","acr_values":["2"],"max_age":600}""")
        val refusal = ApiException(401, "step_up_required", problem = body, authenticationChallenge = "Bearer error=insufficient_user_authentication")
        assertTrue(refusal.needsStrongSignIn); assertEquals("2", refusal.signInRequirement!!.requestedACR)
    }
    @Test fun ordinaryFailuresCannotSupplyAuthenticationAuthority() {
        val body = problem("""{"acr_values":["2"],"max_age":600}""")
        assertFalse(ApiException(401, "unauthorized", problem = body).needsStrongSignIn)
        assertNull(ApiException(403, "step_up_required", problem = body).signInRequirement)
        assertTrue(ApiException(401, "step_up_required").needsStrongSignIn)
        assertNull(ApiException(401, "step_up_required").signInRequirement)
    }
}
