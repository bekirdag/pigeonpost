package dev.pigeonpost.core

/** How an address is drawn: its initials and the tone it keeps, matching the web and iOS apps. */
object PeerFace {
    /** Letters come from the last segment of the shown name, so one fleet's mailboxes differ. */
    fun initials(name: String): String {
        val last = name.split('/').lastOrNull { it.isNotEmpty() } ?: name
        val letters = last.filter { it.isLetterOrDigit() }
        return if (letters.isEmpty()) "··" else letters.take(2).uppercase()
    }

    /** One of six tones, 1..6: `h * 31 + charCode` over UTF-16 units, wrapped at 32 bits as JavaScript's `>>> 0`. */
    fun toneIndex(peer: String?): Int {
        if (peer == null) return 1
        var hash = 0
        for (unit in peer) hash = hash * 31 + unit.code
        return (hash.toUInt() % 6u).toInt() + 1
    }
}
