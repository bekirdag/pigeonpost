package dev.pigeonpost.core

import org.junit.Test
import org.junit.Assert.*

class AddressTest {

    @Test
    fun handlesReadAsNames() {
        assertEquals("Bekir", displayName("/bekir/main"))
        assertEquals("Bekir", displayName("/bekir"))
        assertEquals("Alp", displayName("/alp/main"))
        assertEquals("Main", displayName("/main"))
        assertEquals("Bekir Agent", displayName("/bekir/agent"))
        assertEquals("Wodo HR", displayName("/wodo/hr"))
        assertEquals("Wodo CEO", displayName("/wodo/ceo"))
        assertEquals("Acme John Doe", displayName("/acme/john-doe"))
        assertEquals("Acme John Doe", displayName("/acme/john_doe"))
        assertEquals("Acme QA Bot", displayName("/acme/qa/bot"))
        assertEquals("Team inbox", displayName("Team inbox"))
        assertEquals(listOf("hr", "it", "qa", "ui", "ux", "pr", "ai", "ceo", "cto", "cfo", "ops", "dev"), NAME_ACRONYMS)
    }

    @Test
    fun shapesThatAreNotNamesKeepTheirDisplay() {
        assertEquals("github/alex", displayName("/github/alex"))
        assertEquals("alex@example.com", displayName("/alex@example.com"))
        assertEquals("aaaa111…33dd", displayName("/k/aaaa1111bbbb2222cccc3333dd"))
    }

    @Test
    fun initialsComeFromTheDerivedName() {
        assertEquals("BE", initials(displayName("/bekir/main")))
        assertEquals("WH", initials(displayName("/wodo/hr")))
        assertEquals("AD", initials(displayName("/acme/john-doe")))
        assertEquals("MU", initials("Mum"))
        assertEquals("··", initials(""))
    }

    @Test
    fun conversationInputSuppliesTheSlashWithoutChangingTheDestination() {
        assertEquals("/", conversationAddressInput(""))
        assertFalse(validAddress(conversationAddressInput("")))
        assertEquals("/bekir", conversationAddressInput("bekir"))
        assertEquals("/bekir/main", conversationAddressInput(" /bekir/main "))
        assertEquals("/k/ABC123", conversationAddressInput("k/ABC123"))
        assertFalse(validAddress(conversationAddressInput("https://example.com")))
        assertFalse(validAddress(conversationAddressInput("//bekir")))
    }

    @Test
    fun validSlashAddresses() {
        assertTrue(validAddress("/demo/builder"))
        assertTrue(validAddress("/k/abcdef"))
        assertTrue(validAddress("/demo/main"))
        assertTrue(validAddress("/alex+tag@gmail.com"))
        assertTrue(validAddress("/alex*tag@gmail.com/main"))
        assertFalse(validAddress("/demo/*"))
    }

    @Test
    fun wildcardOptInAndTerminal() {
        assertTrue(validAddress("/demo/*", wildcard = true))
        assertFalse(validAddress("/demo/*", wildcard = false))
        assertFalse(validAddress("/demo/*/agent", wildcard = true))
        assertFalse(validAddress("/demo/*/agent", wildcard = false))
    }

    @Test
    fun traversalAndWhitespaceDenied() {
        assertFalse(validAddress("demo/builder"))
        assertFalse(validAddress("/demo/../builder"))
        assertFalse(validAddress("/demo/./builder"))
        assertFalse(validAddress("/demo/* withoutwildcard"))
        assertFalse(validAddress("/demo/ agent"))
        assertFalse(validAddress(" /demo"))
        assertFalse(validAddress("/demo "))
    }

    @Test
    fun strictIssuerUrls() {
        val issuer = "https://auth.pigeonpost.dev/realms/pigeonpost-prod"
        val safeUrl = "https://auth.pigeonpost.dev/realms/pigeonpost-prod/device?user_code=ABCD"
        assertEquals(safeUrl, verifiedSignInUrl(safeUrl, issuer))
        assertNull(verifiedSignInUrl("https://evil.com", issuer))
        assertNull(verifiedSignInUrl("https://auth.pigeonpost.dev.evil.com", issuer))
        assertNull(verifiedSignInUrl("https://user@auth.pigeonpost.dev", issuer))
        assertNull(verifiedSignInUrl("https://auth.pigeonpost.dev:8443", issuer))
        assertNull(verifiedSignInUrl("https://auth.pigeonpost.dev/#fragment", issuer))
        assertNull(verifiedSignInUrl("javascript:alert(1)", issuer))
    }
}
