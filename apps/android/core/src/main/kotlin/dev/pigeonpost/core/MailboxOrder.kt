package dev.pigeonpost.core

import java.util.Locale

/** A stable picker order, independent of the inbox currently being read. */
fun orderedMailboxes(mailboxes: List<Mailbox>, username: String?): List<Mailbox> {
    data class Row(val index: Int, val mailbox: Mailbox, val path: String, val namespace: String, val named: Boolean, val root: Boolean)
    val rows = mailboxes.mapIndexed { index, mailbox ->
        val path = mailbox.handle.orEmpty().trim().lowercase(Locale.ROOT)
        val parts = path.trim('/').split('/')
        val named = path.startsWith('/') && parts.first().isNotEmpty() && parts.first() != "k"
        val root = named && (parts.size == 1 || (parts.size == 2 && parts.last() == "main"))
        Row(index, mailbox, path.ifEmpty { mailbox.address }, if (named) parts.first() else "", named, root)
    }
    val user = username.orEmpty().trim().trim('/').lowercase(Locale.ROOT)
    val roots = rows.filter { it.root }
    val primary = roots.firstOrNull { user.isNotEmpty() && it.namespace == user } ?: roots.firstOrNull()
    return rows.sortedWith(compareBy<Row> {
        when { it.index == primary?.index -> 0; it.root -> 1; it.named -> 2; else -> 3 }
    }.thenBy { it.namespace }.thenBy { it.path }.thenBy { it.index }).map { it.mailbox }
}
