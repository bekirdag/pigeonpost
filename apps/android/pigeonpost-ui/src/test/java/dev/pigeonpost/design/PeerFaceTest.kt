package dev.pigeonpost.design

import org.junit.Assert.assertEquals
import org.junit.Test

/** Same expectations as the shipped iOS app (apps/ios/Tests/main.swift) and the web inbox. */
class PeerFaceTest {
    @Test fun displayNames() {
        assertEquals("Bekir", PeerFace.displayName("/bekir/main"))
        assertEquals("Bekir", PeerFace.displayName("/bekir"))
        assertEquals("Wodo HR", PeerFace.displayName("/wodo/hr"))
        assertEquals("Acme John Doe", PeerFace.displayName("/acme/john-doe"))
        assertEquals("Acme John Doe", PeerFace.displayName("/acme/john_doe"))
        assertEquals("Acme QA Bot", PeerFace.displayName("/acme/qa/bot"))
        assertEquals("/github/alex", PeerFace.displayName("/github/alex"))
        assertEquals("/alex@example.com", PeerFace.displayName("/alex@example.com"))
        assertEquals("Team inbox", PeerFace.displayName("Team inbox"))
        assertEquals("Main", PeerFace.displayName("/main"))
        assertEquals("/k/aaaa1111b…", PeerFace.displayName("/k/aaaa1111bbbb2222cccc3333dd"))
        assertEquals("unknown", PeerFace.displayName(null))
    }

    @Test fun initials() {
        assertEquals("BE", PeerFace.initials("/bekir/main"))
        assertEquals("WH", PeerFace.initials("/wodo/hr"))
        assertEquals("AD", PeerFace.initials("/acme/john-doe"))
        assertEquals("MU", PeerFace.initials("/bekir/main", name = "Mum"))
        assertEquals("BA", PeerFace.initials("/bekir/agent1"))
        assertEquals("AY", PeerFace.initials("/ada", name = "Ada Yılmaz"))
        assertEquals("··", PeerFace.initials("/", name = "  "))
    }

    @Test fun toneIndexMatchesTheWebApp() {
        assertEquals(6, PeerFace.toneIndex("/bekir/agent1"))
        assertEquals(1, PeerFace.toneIndex("/bekir/docdex"))
        assertEquals(6, PeerFace.toneIndex("/k/eeee5555ffff6666gggg7777hh"))
        assertEquals(1, PeerFace.toneIndex(null))
    }

    @Test fun toneOrderIsNavyBlueTealVioletAmberRed() {
        assertEquals(PPTone.Red, PeerFace.tone("/bekir/agent1"))
        assertEquals(PPTone.Navy, PeerFace.tone("/bekir/docdex"))
    }

    @Test fun toneColoursFollowTheTheme() {
        assertEquals(PPLight.accentSubtle, ppToneColors(PPTone.Blue, PPLight, false).background)
        assertEquals(PPDark.textLink, ppToneColors(PPTone.Blue, PPDark, true).foreground)
        assertEquals(PPLight.brand, ppToneColors(PPTone.Navy, PPLight, false).background)
    }
}
