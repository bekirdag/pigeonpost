package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.rememberScrollState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.collectAsState
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.semantics.selected
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.unit.dp
import dev.pigeonpost.core.Conversation
import dev.pigeonpost.core.InboxState
import dev.pigeonpost.core.ListFilter
import dev.pigeonpost.design.PPIcon
import dev.pigeonpost.design.PPTag
import dev.pigeonpost.inbox.InboxViewModel

/** The quick filters above the conversation list (the web inbox's `#list-filters`): All · Unread · Agents · People. */
@Composable
fun ListFilterChips(selected: ListFilter, select: (ListFilter) -> Unit, modifier: Modifier = Modifier) {
    Row(modifier.horizontalScroll(rememberScrollState()).testTag("listFilters"), horizontalArrangement = Arrangement.spacedBy(6.dp),
        verticalAlignment = Alignment.CenterVertically) {
        ListFilter.entries.forEach { filter ->
            PPTag(filter.label, Modifier.semantics { this.selected = filter == selected }.testTag("listFilter:" + filter.name.lowercase()),
                icon = when (filter) { ListFilter.AGENTS -> PPIcon.Bot; ListFilter.PEOPLE -> PPIcon.User; else -> null },
                selected = filter == selected, onClick = { select(filter) })
        }
    }
}

/** Whether an address is one of the account's Nests (personal or an organization's), once they have loaded. */
@Composable
fun rememberNestCheck(model: InboxViewModel): (String) -> Boolean {
    val nests = model.nests ?: return remember { { _: String -> false } }
    val state by nests.state.collectAsState()
    return remember(state) { { peer: String -> state.nestIdFor(peer) != null } }
}

/** The search-narrowed list, narrowed again by the quick filter. */
fun InboxState.shown(filter: ListFilter, isNest: (String) -> Boolean): List<Conversation> =
    if (filter == ListFilter.ALL) visible else visible.filter { filter.matches(it, isNest) }

/** Whether any group row or invitation stays on the list under this filter. */
fun ListFilter.showsAnyGroup(groups: dev.pigeonpost.core.groups.GroupsState): Boolean =
    (showsInvitations && groups.invitations.isNotEmpty()) || groups.joined.any { showsGroup(it.my?.unread ?: 0) }
