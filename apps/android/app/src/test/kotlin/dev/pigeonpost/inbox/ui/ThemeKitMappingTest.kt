package dev.pigeonpost.inbox.ui

import androidx.compose.ui.graphics.Color
import dev.pigeonpost.design.PPDark
import dev.pigeonpost.design.PPLight
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Test

/** The app's colour names come from the design kit's semantic roles, in both themes. */
class ThemeKitMappingTest {
    @Test fun brandAndAccentAreTheKits() {
        val light = palette(PPLight, false)
        assertEquals(Color(0xFF086CFF), light.accent) // Signal Blue
        assertEquals(Color(0xFF05368B), light.brand) // Pigeon Navy
        assertEquals(Color(0xFF26CE2D), light.agentRail) // Reply Green
    }

    @Test fun bubblesAndTicksFollowTheTheme() {
        for ((kit, dark) in listOf(PPLight to false, PPDark to true)) {
            val t = palette(kit, dark)
            assertEquals(dark, t.dark)
            assertEquals(kit.bubbleMine, t.mineFill)
            assertEquals(kit.bubbleMineBorder, t.mineStroke)
            assertEquals(kit.bubbleMineText, t.mineText)
            assertEquals(kit.tickSeen, t.tickSeen)
            assertEquals(kit.text, t.ink)
            assertEquals(kit.bg, t.app)
            assertEquals(kit.surface, t.ground)
            assertEquals(kit.danger, t.blockedText)
            assertEquals(kit.warning, t.heldText)
        }
        assertNotEquals(palette(PPLight, false).ink, palette(PPDark, true).ink)
    }

    @Test fun sixFaceTonesInTheWebOrder() {
        val t = palette(PPLight, false)
        assertEquals(6, t.tones.size)
        assertEquals(6, t.toneInk.size)
        assertEquals(PPLight.brand, t.tones[0]) // tone 1 is navy
        assertEquals(PPLight.accentSubtle, t.tones[1]) // tone 2 is blue
        assertEquals(PPLight.dangerFill, t.tones[5]) // tone 6 is red
    }

    @Test fun callNotificationAvatarUsesTheSameTones() {
        val (fill, ink) = avatarTone("/wodo/hr", dark = false)
        val index = dev.pigeonpost.core.PeerFace.toneIndex("/wodo/hr") - 1
        assertEquals(palette(PPLight, false).tones[index], fill)
        assertEquals(palette(PPLight, false).toneInk[index], ink)
    }
}
