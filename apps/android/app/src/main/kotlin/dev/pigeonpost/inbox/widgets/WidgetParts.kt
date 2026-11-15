package dev.pigeonpost.inbox.widgets

import androidx.compose.runtime.Composable
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.TextUnit
import androidx.compose.ui.unit.dp
import androidx.glance.ColorFilter
import androidx.glance.GlanceModifier
import androidx.glance.Image
import androidx.glance.ImageProvider
import androidx.glance.action.Action
import androidx.glance.action.clickable
import androidx.glance.appwidget.appWidgetBackground
import androidx.glance.appwidget.cornerRadius
import androidx.glance.background
import androidx.glance.layout.Alignment
import androidx.glance.layout.Box
import androidx.glance.layout.Column
import androidx.glance.layout.Row
import androidx.glance.layout.Spacer
import androidx.glance.layout.fillMaxSize
import androidx.glance.layout.fillMaxWidth
import androidx.glance.layout.height
import androidx.glance.layout.padding
import androidx.glance.layout.size
import androidx.glance.layout.width
import androidx.glance.semantics.contentDescription
import androidx.glance.semantics.semantics
import androidx.glance.text.FontWeight
import androidx.glance.text.Text
import androidx.glance.unit.ColorProvider
import dev.pigeonpost.design.PPIcon
import dev.pigeonpost.design.PeerFace
import dev.pigeonpost.inbox.R
import java.text.DateFormat
import java.time.Instant
import java.time.ZoneId
import java.util.Date

/** The rounded card every widget sits on: kit surface, kit radius, on every Android version. */
@Composable
fun WidgetCard(action: Action, modifier: GlanceModifier = GlanceModifier, padding: Dp = WidgetSpace.gutter, content: @Composable () -> Unit) {
    Box(modifier.fillMaxSize().appWidgetBackground().cornerRadius(WidgetSpace.radius)
        .background(ImageProvider(R.drawable.widget_background), colorFilter = ColorFilter.tint(WidgetColors.surface))
        .clickable(action).padding(padding)) { content() }
}

@Composable
fun WidgetIcon(icon: PPIcon, size: Dp, tint: ColorProvider = WidgetColors.muted, description: String? = null) {
    Image(ImageProvider(icon.res), description, GlanceModifier.size(size), colorFilter = ColorFilter.tint(tint))
}

/** A kit-tinted circle: avatars, unread dots, status dots. */
@Composable
fun Disc(size: Dp, color: ColorProvider, modifier: GlanceModifier = GlanceModifier, content: @Composable () -> Unit = {}) {
    Box(modifier.size(size).background(ImageProvider(R.drawable.widget_circle), colorFilter = ColorFilter.tint(color)), contentAlignment = Alignment.Center) { content() }
}

/** Initials on the peer's tone, as in the app's conversation list (PeerFace). */
@Composable
fun Initials(peer: String, name: String, size: Dp, group: Boolean = false) {
    val (ground, ink) = WidgetColors.face(peer)
    Disc(size, ground) {
        if (group) WidgetIcon(PPIcon.Users, size * 0.5f, ink)
        else Text(PeerFace.initials(peer, name), style = WidgetType.style(if (size >= 36.dp) WidgetType.bodySmall else WidgetType.overline, ink, FontWeight.Bold), maxLines = 1)
    }
}

@Composable
fun UnreadDot(size: Dp = 9.dp) = Disc(size, WidgetColors.accent, GlanceModifier.semantics { contentDescription = "Unread" })

/** The count in a pill: "12", or "99+". */
@Composable
fun CountPill(count: Int, modifier: GlanceModifier = GlanceModifier) {
    Box(modifier.height(22.dp).cornerRadius(11.dp)
        .background(ImageProvider(R.drawable.widget_pill), colorFilter = ColorFilter.tint(WidgetColors.accent)).padding(horizontal = 8.dp),
        contentAlignment = Alignment.Center) {
        Text(if (count > 99) "99+" else count.toString(), style = WidgetType.style(WidgetType.caption, WidgetColors.onAccent, FontWeight.Bold), maxLines = 1)
    }
}

/** A round icon button on the accent: the widgets' one primary action. */
@Composable
fun ActionButton(icon: PPIcon, description: String, action: Action, size: Dp = 36.dp, filled: Boolean = true) {
    Disc(size, if (filled) WidgetColors.accent else WidgetColors.accentSubtle, GlanceModifier.clickable(action).semantics { contentDescription = description }) {
        WidgetIcon(icon, size * 0.5f, if (filled) WidgetColors.onAccent else WidgetColors.link)
    }
}

/** Title row: icon, title, an optional count, and an optional action on the right. */
@Composable
fun WidgetHeader(icon: PPIcon, title: String, count: Int? = null, compact: Boolean = false, trailing: (@Composable () -> Unit)? = null) {
    Row(GlanceModifier.fillMaxWidth(), verticalAlignment = Alignment.CenterVertically) {
        // At 2x2 the title needs the room more than the icon does.
        if (!compact) { WidgetIcon(icon, 18.dp, WidgetColors.link); Spacer(GlanceModifier.width(8.dp)) }
        Text(title, GlanceModifier.defaultWeight(), style = WidgetType.style(if (compact) WidgetType.bodySmall else WidgetType.title, WidgetColors.text, FontWeight.Bold), maxLines = 1)
        if (count != null && count > 0) { CountPill(count); if (trailing != null) Spacer(GlanceModifier.width(8.dp)) }
        trailing?.invoke()
    }
}

/** A centred message for an empty, signed-out or locked widget, with an optional call to action. */
@Composable
fun WidgetMessage(icon: PPIcon, title: String, detail: String? = null, actionLabel: String? = null, action: Action? = null, compact: Boolean = false) {
    Column(GlanceModifier.fillMaxSize(), horizontalAlignment = Alignment.CenterHorizontally, verticalAlignment = Alignment.CenterVertically) {
        if (!compact) { WidgetIcon(icon, 24.dp, WidgetColors.muted); Spacer(GlanceModifier.height(6.dp)) }
        Text(title, style = WidgetType.style(if (compact) WidgetType.bodySmall else WidgetType.body, WidgetColors.text, FontWeight.Medium), maxLines = 2)
        if (detail != null && !compact) {
            Spacer(GlanceModifier.height(2.dp))
            Text(detail, style = WidgetType.style(WidgetType.caption, WidgetColors.muted), maxLines = 3)
        }
        if (actionLabel != null && action != null) {
            Spacer(GlanceModifier.height(8.dp))
            Box(GlanceModifier.height(32.dp).cornerRadius(16.dp).background(ImageProvider(R.drawable.widget_pill), colorFilter = ColorFilter.tint(WidgetColors.accent))
                .clickable(action).padding(horizontal = 14.dp), contentAlignment = Alignment.Center) {
                Text(actionLabel, style = WidgetType.style(WidgetType.bodySmall, WidgetColors.onAccent, FontWeight.Medium), maxLines = 1)
            }
        }
    }
}

/** A thin rounded bar: Nest running hours. Drawn from boxes so it looks the same on every version. */
@Composable
fun Meter(fraction: Float, width: Dp, color: ColorProvider = WidgetColors.accent) {
    val filled = (width.value * fraction.coerceIn(0f, 1f)).dp
    Box(GlanceModifier.width(width).height(6.dp).background(ImageProvider(R.drawable.widget_pill), colorFilter = ColorFilter.tint(WidgetColors.track))) {
        if (filled > 0.dp) Box(GlanceModifier.width(maxOf(filled, 6.dp)).height(6.dp).background(ImageProvider(R.drawable.widget_pill), colorFilter = ColorFilter.tint(color))) {}
    }
}

@Composable
fun Label(text: String, size: TextUnit = WidgetType.caption, color: ColorProvider = WidgetColors.muted, weight: FontWeight = FontWeight.Normal, modifier: GlanceModifier = GlanceModifier) =
    Text(text, modifier, style = WidgetType.style(size, color, weight), maxLines = 1)

/** Today: the time; otherwise the date, as the app's conversation list shows it. */
fun widgetTime(epoch: Long, now: Long, zone: ZoneId = ZoneId.systemDefault()): String = if (epoch <= 0) "" else {
    val today = Instant.ofEpochSecond(now).atZone(zone).toLocalDate()
    val date = Instant.ofEpochSecond(epoch).atZone(zone).toLocalDate()
    val format = if (today == date) DateFormat.getTimeInstance(DateFormat.SHORT) else DateFormat.getDateInstance(DateFormat.SHORT)
    format.timeZone = java.util.TimeZone.getTimeZone(zone)
    format.format(Date(epoch * 1000))
}
