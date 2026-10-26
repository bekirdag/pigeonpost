package dev.pigeonpost.core.groups

/** Local projections use verified encrypted references; server replacement hints grant no authority. */
object GroupActions {
    fun sequence(value: String?): Long? = value?.toLongOrNull()?.takeIf { it > 0 && it.toString() == value }
    private fun indexed(history: List<HistoryItem>) = buildMap<Long, HistoryItem> {
        history.filter { it.kind == "message" }.forEach { item -> item.seq?.let { if (it !in this) put(it, item) } }
    }
    private fun parent(item: HistoryItem, bySeq: Map<Long, HistoryItem>): HistoryItem? {
        if (item.kind != "message" || !item.actionsVerified || item.pending) return null
        val seq = item.seq ?: return null
        val parentSeq = sequence(item.editOf)?.takeIf { it < seq } ?: return null
        val sender = item.sender ?: return null
        return bySeq[parentSeq]?.takeIf { it.sender == sender && !it.deleted && !it.undecryptable && it.text != null }
    }
    fun parent(item: HistoryItem, history: List<HistoryItem>) = parent(item, indexed(history))
    fun versions(item: HistoryItem, history: List<HistoryItem>): List<HistoryItem> {
        val bySeq = indexed(history)
        val chain = mutableListOf(item)
        var current = item
        while (true) {
            current = parent(current, bySeq) ?: break
            chain += current
        }
        return chain.asReversed()
    }
    fun visible(history: List<HistoryItem>, now: Long): List<HistoryItem> {
        val bySeq = indexed(history)
        val replaced = mutableSetOf<Long>()
        val deadlines = mutableMapOf<Long, Long>()
        for (item in history.sortedBy { it.seq ?: Long.MAX_VALUE }) {
            if (item.kind != "message") continue
            val seq = item.seq ?: continue
            var deadline = item.expiresAt ?: Long.MAX_VALUE
            parent(item, bySeq)?.let { root ->
                root.seq?.let { parentSeq ->
                    replaced += parentSeq
                    deadline = minOf(deadline, deadlines[parentSeq] ?: root.expiresAt ?: Long.MAX_VALUE)
                }
            }
            deadlines[seq] = deadline
        }
        return history.filter { item ->
            val seq = item.seq
            (item.kind != "message" || seq !in replaced) &&
                (if (item.kind == "message") deadlines[seq] ?: item.expiresAt ?: Long.MAX_VALUE else item.expiresAt ?: Long.MAX_VALUE) > now
        }
    }
    fun editable(item: HistoryItem, history: List<HistoryItem>, me: String, now: Long): Boolean {
        if (item.kind != "message" || item.sender != me || item.seq == null || item.pending || item.deleted || item.undecryptable || item.text == null) return false
        if (visible(history, now).none { it.seq == item.seq && it.kind == "message" }) return false
        val first = versions(item, history).first()
        return first.editOf == null && first.createdAt > 0 && now >= first.createdAt && now - first.createdAt < 86_400
    }
    fun linkedSequences(history: List<HistoryItem>, seeds: Set<Long>): Set<Long> {
        val bySeq = indexed(history)
        val edges = mutableMapOf<Long, MutableList<Long>>()
        for (item in history) {
            if (item.kind != "message" || !item.actionsVerified) continue
            val seq = item.seq ?: continue
            val previous = sequence(item.editOf)?.takeIf { it < seq } ?: continue
            val sender = item.sender ?: continue
            if (bySeq[previous]?.sender != sender) continue
            edges.getOrPut(seq) { mutableListOf() }.add(previous)
            edges.getOrPut(previous) { mutableListOf() }.add(seq)
        }
        val found = seeds.filterTo(mutableSetOf()) { it > 0 }
        val queue = found.toMutableList()
        var index = 0
        while (index < queue.size) {
            for (next in edges[queue[index++]].orEmpty()) if (found.add(next)) queue += next
        }
        return found
    }
    fun erasing(history: List<HistoryItem>, seqs: Set<Long>, tombstones: Boolean = true): List<HistoryItem> {
        val erased = linkedSequences(history, seqs)
        return history.mapNotNull { item ->
            if (item.seq !in erased && !(item.pending && sequence(item.editOf) in erased)) item
            else if (tombstones && item.kind == "message" && item.seq != null)
                HistoryItem(kind = "message", seq = item.seq, createdAt = item.createdAt, expiresAt = item.expiresAt, sender = item.sender, deleted = true)
            else null
        }
    }
    fun pruning(history: List<HistoryItem>, now: Long): List<HistoryItem> {
        val expired = history.filter { (it.expiresAt ?: Long.MAX_VALUE) <= now }.mapNotNull { it.seq }.toSet()
        return erasing(history, expired, tombstones = false).filter { (it.expiresAt ?: Long.MAX_VALUE) > now }
    }
    fun receipt(item: HistoryItem, receipts: GroupReceipts?): String {
        val seq = item.seq ?: return "sending"
        val members = receipts?.members.orEmpty()
        if (members.isEmpty() || members.any { it.deliveredSeq < seq }) return "sent"
        val visible = if (receipts?.readReceipts == true) members.mapNotNull { it.seenSeq } else emptyList()
        return if (visible.isNotEmpty() && visible.all { it >= seq }) "seen" else "delivered"
    }
}
