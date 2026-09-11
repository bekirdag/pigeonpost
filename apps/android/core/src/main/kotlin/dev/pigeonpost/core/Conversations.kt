package dev.pigeonpost.core

data class Conversation(
    val peer: String,
    val messages: List<ThreadMessage>,
    val contact: Contact? = null,
    val identity: Mailbox? = null,
) {
    val mine get() = identity != null
    val name get() = identity?.name ?: contact?.alias?.takeIf { it.isNotBlank() } ?: displayName(peer)
    val unread get() = messages.count { !it.outgoing && !it.read }
    val held get() = messages.count { !it.outgoing && it.autonomy == "review" && it.verb != null }
    val last get() = messages.lastOrNull()?.at ?: 0
    val blocked get() = contact?.admission == "block"
}
data class Subject(val id: String, val title: String? = null, val isDefault: Boolean = false, val messages: List<ThreadMessage> = emptyList(), val lastAt: Long = 0) {
    val name get() = title?.takeIf { it.isNotBlank() } ?: if (isDefault || id.isEmpty()) "General" else "Untitled"
    val unread get() = messages.count { !it.outgoing && !it.read }
    val last get() = maxOf(lastAt, messages.maxOfOrNull { it.at } ?: 0)
}

object Conversations {
    fun aliases(messages: List<Message>): Map<String, String> = buildMap {
        messages.forEach { message ->
            val handle = message.peerHandle ?: message.senderHandle ?: return@forEach
            message.peer?.let { put(it, handle) }
            if (!message.outgoing) message.from?.let { put(it, handle) }
            if (message.outgoing) message.to?.let { put(it, handle) }
        }
    }

    fun contact(peer: String, contacts: List<Contact>): Contact? = contacts.firstOrNull { it.peer == peer }
        ?: peer.trim('/').split('/').takeIf { it.size >= 2 }?.let { parts -> contacts.firstOrNull { it.peer == "/${parts[0]}/*" } }

    fun build(messages: List<Message>, pending: List<PendingMessage>, contacts: List<Contact>, mailboxes: List<Mailbox>, acting: String?): List<Conversation> {
        val aliases = aliases(messages)
        val grouped = linkedMapOf<String, MutableList<ThreadMessage>>()
        val seen = mutableSetOf<String>()
        messages.forEach { message ->
            if (!seen.add(message.id)) return@forEach
            val peer = aliases[message.peerKey] ?: message.peerKey
            grouped.getOrPut(peer) { mutableListOf() }.add(ThreadMessage(
                id = message.id, body = message.body, outgoing = message.outgoing, at = message.at, threadId = message.threadId,
                read = message.outgoing || message.read == true,
                autonomy = message.autonomy.takeUnless { message.outgoing }, verb = message.verb.takeUnless { message.outgoing },
                heldBecause = message.heldBecause.takeUnless { message.outgoing }, address = message.from,
                standing = message.standing, tier = message.tier, attachments = message.attachments.orEmpty(),
            ))
        }
        pending.filter { it.mailbox == acting && it.id !in seen && it.sentCopyId !in seen }.forEach { row ->
            val peer = aliases[row.to] ?: row.to
            grouped.getOrPut(peer) { mutableListOf() }.add(ThreadMessage(row.id, row.body, true, row.at, row.threadId, status = row.status, attachments = row.attachments))
        }
        contacts.filterNot { it.wildcard }.forEach { grouped.getOrPut(aliases[it.peer] ?: it.peer) { mutableListOf() } }
        return grouped.map { (peer, rows) ->
            Conversation(peer, rows.sortedWith(compareBy<ThreadMessage> { it.at }.thenBy { it.id }), contact(peer, contacts), mailboxes.firstOrNull { it.address != acting && (it.key == peer || aliases[it.address] == peer) })
        }.sortedWith(compareByDescending<Conversation> { it.last }.thenBy(String.CASE_INSENSITIVE_ORDER) { it.name }.thenBy { it.peer })
    }

    fun subjects(conversation: Conversation?, threads: List<ServerThread>, peer: String, messages: List<Message>): List<Subject> {
        val aliases = aliases(messages)
        val groups = linkedMapOf<String, Subject>()
        conversation?.messages.orEmpty().groupBy { it.threadId ?: "" }.forEach { (id, rows) -> groups[id] = Subject(id, isDefault = id.isEmpty(), messages = rows) }
        threads.filter { (aliases[it.peer] ?: it.peer) == peer }.forEach { thread ->
            groups[thread.id] = (groups[thread.id] ?: Subject(thread.id)).copy(title = thread.title, isDefault = thread.isDefault == true, lastAt = thread.lastAt ?: 0)
        }
        val default = groups.values.firstOrNull { it.id.isNotEmpty() && it.isDefault }
        if (default != null) groups.remove("")?.let { legacy ->
            groups[default.id] = default.copy(messages = (default.messages + legacy.messages).distinctBy { it.id }.sortedWith(compareBy<ThreadMessage> { it.at }.thenBy { it.id }), lastAt = maxOf(default.last, legacy.last))
        }
        // A new named subject must not strand a draft in the general conversation.
        if (groups.values.none { it.isDefault }) groups[""] = Subject("", isDefault = true)
        return groups.values.sortedWith(compareByDescending<Subject> { it.last }.thenBy { it.id }).ifEmpty { listOf(Subject("", isDefault = true)) }
    }

    fun targetThread(subjects: List<Subject>, selected: String?): String? =
        (subjects.firstOrNull { it.id == selected } ?: subjects.firstOrNull())?.id?.takeIf { it.isNotEmpty() }
}
