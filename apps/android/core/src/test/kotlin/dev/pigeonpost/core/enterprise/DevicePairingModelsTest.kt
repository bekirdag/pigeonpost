package dev.pigeonpost.core.enterprise

import dev.pigeonpost.core.WireJson
import org.junit.Assert.*
import org.junit.Test
import java.util.Locale

class DevicePairingModelsTest {
    private val now = 1_800_000_000L
    private fun preview() = DevicePairingPreview("pair_fixture", PairingOrganization("org_fixture", "Example"), "/example/build", "/k/fixture", false,
        "Build machine", "123 456", listOf("mailbox"), requestedAt = now - 30, expiresAt = now + 570, canApprove = true, state = "pending")
    @Test fun codesNormalizeOnlyBoundedAsciiBeforeCaseConversion() {
        val previous = Locale.getDefault()
        try {
            Locale.setDefault(Locale.forLanguageTag("tr-TR"))
            assertEquals("ABCD-123I", DevicePairingCode.parse(" abcd123i \n")!!.displayValue)
            assertEquals(DevicePairingCode.parse("ABCD1234"), DevicePairingCode.parse("abcd-1234"))
        } finally { Locale.setDefault(previous) }
        for (value in listOf("", "ABCD123", "ABCD12345", "AB-CD1234", "ABCD--1234", "ABCD 1234", "ABCD/1234", "ABCD\n1234", "ABCD12İ4", "ABCD1ß4", "ＡＢＣＤ1234", "\u2003ABCD1234", " ".repeat(129) + "ABCD1234"))
            assertNull(value, DevicePairingCode.parse(value))
        assertFalse(DevicePairingCode.parse("ABCD1234").toString().contains("ABCD"))
    }
    @Test fun linksAcceptOnlyTheExactPublishedOriginPathAndFragment() {
        assertEquals("ABCD-1234", DevicePairingLink.parse("https://inbox.pigeonpost.dev/pair.html#abcd1234")!!.displayValue)
        for (url in listOf("http://inbox.pigeonpost.dev/pair.html#ABCD1234", "https://untrusted.invalid/pair.html#ABCD1234", "https://inbox.pigeonpost.dev.untrusted.invalid/pair.html#ABCD1234", "https://user@inbox.pigeonpost.dev/pair.html#ABCD1234", "https://inbox.pigeonpost.dev:443/pair.html#ABCD1234", "https://inbox.pigeonpost.dev/pair.html?next=x#ABCD1234", "https://inbox.pigeonpost.dev/pair.html/#ABCD1234", "https://inbox.pigeonpost.dev/a/../pair.html#ABCD1234", "https://inbox.pigeonpost.dev/pair.html#ABCD%2D1234", "https://inbox.pigeonpost.dev/pair.html#ABCD-1234&target=x", "https://inbox.pigeonpost.dev/pair.html", "https://inbox.pigeonpost.dev/m/ABCD1234"))
            assertNull(url, DevicePairingLink.parse(url))
    }
    @Test fun bothExistingAndExplicitNewAgentTargetsAreValidated() {
        val existing = preview(); assertTrue(existing.isValid); assertTrue(existing.allowsApproval(now))
        assertTrue(existing.copy(address = null, createsAgent = true).allowsApproval(now))
        assertFalse(existing.copy(address = null, createsAgent = null).isValid)
        assertFalse(existing.copy(createsAgent = true).isValid)
        assertFalse(existing.copy(address = "/example/other").isValid)
    }
    @Test fun malformedTargetsMetadataAndConfirmationsAreNotActionable() {
        val p = preview()
        val invalid = listOf(p.copy(pairingId = "../escape"), p.copy(org = p.org.copy(id = "bad/id")), p.copy(handle = "/example/../build"),
            p.copy(handle = "/k/fixture"), p.copy(label = "Build\nadmin"), p.copy(label = "Build\u202etarget"), p.copy(confirmation = "１２３４５６"),
            p.copy(confirmation = "12 3456"), p.copy(confirmation = "123 456 "), p.copy(capabilities = emptyList()), p.copy(capabilities = listOf("mailbox", "mailbox")),
            p.copy(capabilities = listOf("mailbox\n")), p.copy(requestedAt = -1), p.copy(expiresAt = p.requestedAt), p.copy(expiresAt = 253_402_300_800L), p.copy(state = "unknown"))
        invalid.forEach { assertFalse(it.toString(), it.isValid); assertFalse(it.allowsApproval(now)) }
        assertFalse(p.toString().contains("123")); assertFalse(p.toString().contains("Build machine")); assertFalse(p.toString().contains("/example"))
    }
    @Test fun permissionTerminalStateExpiryAndFutureClocksAreRechecked() {
        val p = preview()
        assertFalse(p.copy(canApprove = false).allowsApproval(now))
        assertTrue(p.copy(canApprove = false).allowsDecision(now))
        for (state in listOf("approved", "denied")) assertFalse(p.copy(state = state).allowsDecision(now))
        assertFalse(p.allowsApproval(p.expiresAt)); assertFalse(p.copy(requestedAt = now + 61).allowsApproval(now))
        assertTrue(p.copy(state = null).allowsApproval(now)) // Explicit can_approve from older postboxes.
    }
    @Test fun unknownCapabilitiesCanBeReadButNeverBecomeAnApproval() {
        val p = preview().copy(capabilities = listOf("mailbox", "future_privilege"))
        assertTrue(p.isValid); assertFalse(p.allowsApproval(now))
    }
    @Test fun unicodeDisplayBoundsUseUtf8Bytes() {
        assertTrue(preview().copy(org = PairingOrganization("org_fixture", "ü".repeat(160))).isValid)
        assertFalse(preview().copy(org = PairingOrganization("org_fixture", "ü".repeat(161))).isValid)
    }
    @Test fun aMissingPermissionGrantDoesNotDefaultToAllowed() {
        val text = """{"pairing_id":"pair_fixture","org":{"id":"org_fixture","name":"Example"},"handle":"/example/build","address":"/k/fixture","confirmation":"123456","capabilities":["mailbox"],"requested_at":1799999970,"expires_at":1800000570}"""
        val p = WireJson.decodeFromString<DevicePairingPreview>(text)
        assertTrue(p.isValid); assertFalse(p.canApprove); assertFalse(p.allowsApproval(now))
    }
}
