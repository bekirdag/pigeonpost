package dev.pigeonpost.core

/** How an address is drawn: its initials and the tone it keeps, matching the web and iOS apps. */
object PeerFace {
    /** Letters come from the shown name ("Wodo HR" is "WH", "Bekir" is "BE"); a path keeps its last segment's. */
    fun initials(name: String): String = dev.pigeonpost.core.initials(name)

    /** One of six tones, 1..6: `h * 31 + charCode` over UTF-16 units, wrapped at 32 bits as JavaScript's `>>> 0`. */
    fun toneIndex(peer: String?): Int {
        if (peer == null) return 1
        var hash = 0
        for (unit in peer) hash = hash * 31 + unit.code
        return (hash.toUInt() % 6u).toInt() + 1
    }
}
