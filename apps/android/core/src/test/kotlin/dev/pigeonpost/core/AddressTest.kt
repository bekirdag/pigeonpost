package dev.pigeonpost.core

import org.junit.Test
import org.junit.Assert.*

class AddressTest {

    @Test
    fun validSlashAddresses() {
        assertTrue(validAddress("/demo/builder"))
        assertTrue(validAddress("/k/abcdef"))
        assertTrue(validAddress("/demo/main"))
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
