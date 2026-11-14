package dev.pigeonpost.design

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.NavigationBarItemDefaults
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableIntStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.sp

/** One bottom-bar destination. [badge]: 0 hides, -1 shows a dot, n shows a count. */
data class PPTabItem(val icon: PPIcon, val label: String, val badge: Int = 0)

/**
 * Bottom navigation (css .pp-tabbar / Material 3 navigation bar) in kit colours: selected items use
 * the link colour on an accent-subtle pill, badges are danger red.
 */
@Composable
fun PPTabBar(
    items: List<PPTabItem>,
    selected: Int,
    onSelect: (Int) -> Unit,
    modifier: Modifier = Modifier,
) {
    val c = PP.colors
    Column(modifier.fillMaxWidth()) {
        Box(Modifier.fillMaxWidth().height(1.dp).background(c.border))
        NavigationBar(containerColor = c.surface, tonalElevation = 0.dp) {
            items.forEachIndexed { i, item ->
                NavigationBarItem(
                    selected = i == selected,
                    onClick = { onSelect(i) },
                    icon = {
                        BadgedBox(badge = {
                            when {
                                item.badge < 0 -> Badge(containerColor = c.dangerSolid)
                                item.badge > 0 -> Badge(
                                    containerColor = c.dangerSolid,
                                    contentColor = c.onSolid,
                                    modifier = Modifier.semantics { contentDescription = "${item.badge} unread" },
                                ) { Text(if (item.badge > 99) "99+" else "${item.badge}", fontSize = 11.sp) }
                            }
                        }) { PPIconView(item.icon, size = 24.dp) }
                    },
                    label = { Text(item.label, style = PP.type.caption.copy(fontWeight = FontWeight.Medium)) },
                    colors = NavigationBarItemDefaults.colors(
                        selectedIconColor = c.textLink,
                        selectedTextColor = c.textLink,
                        indicatorColor = c.accentSubtle,
                        unselectedIconColor = c.textFaint,
                        unselectedTextColor = c.textMuted,
                    ),
                )
            }
        }
    }
}

/** The default Pigeonpost app destinations. */
val PPDefaultTabs = listOf(
    PPTabItem(PPIcon.Inbox, "Inbox", badge = 3),
    PPTabItem(PPIcon.Phone, "Calls"),
    PPTabItem(PPIcon.Bird, "Nests", badge = -1),
    PPTabItem(PPIcon.Settings, "Settings"),
)

@PPPreviews
@Composable
private fun TabBarPreview() = PPPreviewSurface {
    var sel by remember { mutableIntStateOf(0) }
    PPTabBar(PPDefaultTabs, sel, { sel = it })
}
