package dev.pigeonpost.core

/** Newest-first display IDs over the account snapshot; paging never changes the network contract. */
data class HistoryWindow(val ids: List<String> = emptyList(), val pageSize: Int = 10) {
    init { require(pageSize > 0) }

    fun latest(chronological: List<String>) = copy(ids = chronological.takeLast(pageSize).asReversed())

    fun update(chronological: List<String>, followingLatest: Boolean): HistoryWindow {
        if (followingLatest || ids.isEmpty()) return latest(chronological)
        val retained = ids.toSet()
        return copy(ids = chronological.asReversed().filter { it in retained })
    }

    fun older(chronological: List<String>): HistoryWindow {
        val end = chronological.indexOf(ids.lastOrNull())
        if (end <= 0) return this
        return copy(ids = ids + chronological.subList(maxOf(0, end - pageSize), end).asReversed())
    }

    /** Search can expose older pages directly, with one page of context before the match. */
    fun reveal(chronological: List<String>, target: String): HistoryWindow {
        val match = chronological.indexOf(target)
        if (match < 0) return this
        val existing = chronological.indexOf(ids.lastOrNull()).takeIf { it >= 0 } ?: match
        val start = maxOf(0, minOf(existing, match - pageSize + 1))
        return copy(ids = chronological.drop(start).asReversed())
    }
}
