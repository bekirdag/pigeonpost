package dev.pigeonpost.design

import androidx.compose.ui.graphics.Color

/**
 * How an address is shown: its name, its initials and the tone it keeps. A port of the shipped
 * apps' PeerFace (iOS) and site-inbox/names.js: the tone hash is the web app's, character for
 * character, so one peer has the same face in the browser and on the phone.
 */
object PeerFace {
    /** Short segments that read as acronyms: "/wodo/hr" is "Wodo HR". Same list as names.js. */
    val acronyms: Set<String> = setOf("hr", "it", "qa", "ui", "ux", "pr", "ai", "ceo", "cto", "cfo", "ops", "dev")

    /**
     * A handle as a person's name: "/bekir/main" and "/bekir" are "Bekir", "/wodo/hr" is "Wodo HR",
     * "/acme/john-doe" is "Acme John Doe". Key addresses are truncated, address-style handles and
     * GitHub logins keep their display, and anything that is not a handle is returned as it is.
     */
    fun displayName(peer: String?): String {
        if (peer.isNullOrEmpty()) return "unknown"
        if (peer.startsWith("/k/")) return peer.take(12) + "…"
        val parts = peer.split('/').filter { it.isNotEmpty() }.toMutableList()
        if (!peer.startsWith("/") || parts.isEmpty()) return peer
        if (parts.size > 1 && parts.last() == "main") parts.removeAt(parts.lastIndex)
        if (peer.contains("@") || parts.first() == "github") return "/" + parts.joinToString("/")
        val name = parts.map { part ->
            part.split('-', '_').filter { it.isNotEmpty() }.joinToString(" ") { word ->
                val lower = word.lowercase()
                if (lower in acronyms) lower.uppercase() else lower.take(1).uppercase() + lower.drop(1)
            }
        }.filter { it.isNotEmpty() }.joinToString(" ")
        return name.ifEmpty { peer }
    }

    /**
     * Two letters for a face, from the name it shows: "Bekir" is "BE", "Wodo HR" is "WH" (first and
     * last words). An alias in [name] wins over the handle. A name that is still a path is lettered
     * from its last segment.
     */
    fun initials(peer: String?, name: String? = null): String {
        val shown = (if (!name.isNullOrEmpty()) name else displayName(peer)).trim()
        val base = if (shown.contains('/')) shown.split('/').lastOrNull { it.isNotEmpty() } ?: shown else shown
        val words = base.split(Regex("[\\s\\-_]+"))
            .map { w -> w.filter { it.isLetterOrDigit() } }
            .filter { it.isNotEmpty() }
        val first = words.firstOrNull() ?: return "··"
        val letters = if (words.size > 1) first.take(1) + words.last().take(1) else first.take(2)
        return letters.uppercase()
    }

    /**
     * Which of the six tones a peer keeps, 1..6 — the web app's `data-tone` numbering. The hash is
     * `h * 31 + charCode` over UTF-16 units, wrapping at 32 bits as JavaScript's `>>> 0` does.
     */
    fun toneIndex(peer: String?): Int {
        if (peer == null) return 1
        var hash = 0
        for (unit in peer) hash = hash * 31 + unit.code
        return ((hash.toLong() and 0xFFFFFFFFL) % 6).toInt() + 1
    }

    /** The kit tone for a peer: 1..6 are navy, blue, teal, violet, amber, red. */
    fun tone(peer: String?): PPTone = HASH_TONES[toneIndex(peer) - 1]

    private val HASH_TONES = listOf(PPTone.Navy, PPTone.Blue, PPTone.Teal, PPTone.Violet, PPTone.Amber, PPTone.Red)
}

/** Avatar / initials tones (css .pp-initials[data-tone]). */
enum class PPTone { Blue, Green, Navy, Violet, Amber, Teal, Red, Gray }

data class PPToneColors(val background: Color, val foreground: Color, val border: Color? = null)

/** Resolves a tone against a palette; violet and teal are the web kit's color-mix results, precomputed. */
fun ppToneColors(tone: PPTone, c: PPColors, dark: Boolean): PPToneColors = when (tone) {
    PPTone.Blue -> PPToneColors(c.accentSubtle, c.textLink)
    PPTone.Green -> PPToneColors(c.successFill, c.success)
    PPTone.Navy -> PPToneColors(c.brand, c.surface)
    PPTone.Violet -> if (dark) PPToneColors(Color(0xFF2F2E4A), Color(0xFF866EC5)) else PPToneColors(Color(0xFFEBE7FC), Color(0xFF422C84))
    PPTone.Amber -> PPToneColors(c.warningFill, c.warning)
    PPTone.Teal -> if (dark) PPToneColors(Color(0xFF1D3B3E), Color(0xFF468880)) else PPToneColors(Color(0xFFD9F4F0), Color(0xFF0E5049))
    PPTone.Red -> PPToneColors(c.dangerFill, c.danger)
    PPTone.Gray -> PPToneColors(c.surfaceSunken, c.textMuted, c.border)
}
