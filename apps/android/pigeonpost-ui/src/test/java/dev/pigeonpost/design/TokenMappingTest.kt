package dev.pigeonpost.design

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.toArgb
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Test

class TokenMappingTest {
    private fun hex(c: Color) = "%08X".format(c.toArgb())

    @Test fun brandColoursMatchTokensCss() {
        assertEquals("FF086CFF", hex(PPLight.accent))
        assertEquals("FF086CFF", hex(PPDark.accent))
        assertEquals("FF05368B", hex(PPLight.brand))
        assertEquals("FF8FB4FF", hex(PPDark.brand))
        assertEquals("FF26CE2D", hex(PPLight.bubbleAgentRail))
    }

    @Test fun surfacesFlipBetweenThemes() {
        assertEquals("FFF4F6F8", hex(PPLight.bg))
        assertEquals("FF0B0E12", hex(PPDark.bg))
        assertEquals("FFFFFFFF", hex(PPLight.surface))
        assertEquals("FF141820", hex(PPDark.surface))
        assertNotEquals(PPLight.text, PPDark.text)
    }

    @Test fun messagingTokens() {
        assertEquals("FFEAF1FE", hex(PPLight.bubbleMine))
        assertEquals("FF17325E", hex(PPDark.bubbleMine))
        assertEquals("FF1E8A50", hex(PPLight.tickSeen))
        assertEquals(18.dp, PPRadius.bubble)
        assertEquals(6.dp, PPRadius.bubbletail)
    }

    @Test fun scales() {
        assertEquals(16.dp, PPSpace.s4)
        assertEquals(38.dp, PPControl.m)
        assertEquals(44.dp, PPControl.touchMin)
        assertEquals(15.sp, PPType.body)
        assertEquals(22.sp, PPLineHeight.body)
        assertEquals(PPControl.l, PPButtonSize.L.height)
    }

    @Test fun materialSchemeMapsSemanticRoles() {
        val light = ppColorScheme(PPLight, dark = false)
        assertEquals(PPLight.accent, light.primary)
        assertEquals(PPLight.onAccent, light.onPrimary)
        assertEquals(PPLight.bg, light.background)
        assertEquals(PPLight.surface, light.surface)
        assertEquals(PPLight.dangerSolid, light.error)
        assertEquals(PPLight.borderStrong, light.outline)
        val dark = ppColorScheme(PPDark, dark = true)
        assertEquals(PPDark.surface, dark.surface)
        assertEquals(PPDark.text, dark.onSurface)
        assertEquals(PPEffects.scrimDark, dark.scrim)
    }

    @Test fun bubbleShapeTightensTowardTheSender() {
        val mine = ppBubbleShape(mine = true, position = PPBubblePosition.Middle)
        val theirs = ppBubbleShape(mine = false, position = PPBubblePosition.Single)
        // Shapes are equal only when every corner matches; compare against hand-built expectations.
        assertEquals(androidx.compose.foundation.shape.RoundedCornerShape(topStart = 18.dp, topEnd = 6.dp, bottomEnd = 6.dp, bottomStart = 18.dp), mine)
        assertEquals(androidx.compose.foundation.shape.RoundedCornerShape(topStart = 18.dp, topEnd = 18.dp, bottomEnd = 18.dp, bottomStart = 6.dp), theirs)
    }

    @Test fun iconsResolveByKey() {
        assertEquals(PPIcon.CheckCheck, PPIcon.of("check-check"))
        assertEquals(181, PPIcon.entries.size)
    }
}
