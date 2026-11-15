package dev.pigeonpost.design

import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.RowScope
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.height
import androidx.compose.foundation.layout.offset
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.draw.drawBehind
import androidx.compose.ui.draw.shadow
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Brush
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.graphics.PathEffect
import androidx.compose.ui.graphics.drawscope.Stroke
import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.contentDescription
import androidx.compose.ui.semantics.heading
import androidx.compose.ui.semantics.semantics
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.text.withStyle
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import androidx.compose.ui.unit.sp

/** A pricing feature line; [included] false renders it faint with an x. */
data class PPFeature(val text: String, val included: Boolean = true)

/**
 * Pricing card (css .pp-price-card): name, description, big tabular price + unit, CTA and a feature
 * list. [featured] adds the accent ring, top glow and the [badge] pill ("Most popular").
 */
@Composable
fun PPPricingCard(
    name: String,
    price: String,
    unit: String,
    features: List<PPFeature>,
    ctaLabel: String,
    onCta: () -> Unit,
    modifier: Modifier = Modifier,
    description: String? = null,
    note: String? = null,
    icon: PPIcon? = null,
    featured: Boolean = false,
    badge: String? = null,
    current: Boolean = false,
) {
    val c = PP.colors
    val shape = RoundedCornerShape(PPRadius.xl)
    Box(modifier.padding(top = if (badge != null) 11.dp else 0.dp)) {
        Column(
            Modifier
                .fillMaxWidth()
                .shadow(if (featured) 12.dp else 1.dp, shape, ambientColor = Color(0x1F12161C), spotColor = Color(0x1F12161C))
                .clip(shape)
                .background(c.surface)
                .then(if (featured) Modifier.drawBehind { drawRect(Brush.verticalGradient(listOf(c.accentSubtle, c.surface), startY = 0f, endY = 140.dp.toPx())) } else Modifier)
                .border(if (featured) 2.dp else 1.dp, if (featured) c.accent else c.border, shape)
                .padding(24.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp),
        ) {
            Column(verticalArrangement = Arrangement.spacedBy(6.dp)) {
                Row(Modifier.semantics(mergeDescendants = true) { heading() }, verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                    if (icon != null) PPIconView(icon, size = 20.dp, tint = c.accent)
                    Text(name, style = PP.type.h4, color = c.text)
                }
                if (description != null) Text(description, style = PP.type.bodyS, color = c.textMuted)
            }
            Column {
                Row(Modifier.semantics(mergeDescendants = true) { contentDescription = "$price $unit" }, verticalAlignment = Alignment.Bottom, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                    Text(price, style = PP.type.displayM.copy(fontSize = 40.sp, lineHeight = 44.sp, letterSpacing = (-0.02).em, fontFeatureSettings = "tnum"), color = c.text)
                    Text(unit, Modifier.padding(bottom = 7.dp), style = PP.type.bodyS, color = c.textMuted)
                }
                if (note != null) Text(note, Modifier.padding(top = 2.dp), style = PP.type.caption, color = c.textFaint)
            }
            if (current) {
                PPButton("Current plan", {}, variant = PPButtonVariant.Outline, size = PPButtonSize.L, fullWidth = true, enabled = false)
            } else {
                PPButton(ctaLabel, onCta, variant = if (featured) PPButtonVariant.Primary else PPButtonVariant.Default, size = PPButtonSize.L, fullWidth = true)
            }
            Box(Modifier.fillMaxWidth().height(1.dp).background(c.borderSubtle))
            Column(verticalArrangement = Arrangement.spacedBy(10.dp)) {
                features.forEach { f ->
                    Row(
                        Modifier.semantics(mergeDescendants = true) { contentDescription = (if (f.included) "Included: " else "Not included: ") + f.text },
                        horizontalArrangement = Arrangement.spacedBy(10.dp),
                    ) {
                        PPIconView(if (f.included) PPIcon.Check else PPIcon.X, Modifier.padding(top = 1.dp), size = 18.dp, tint = if (f.included) c.success else c.textFaint)
                        Text(f.text, style = PP.type.bodyS, color = if (f.included) c.textBody else c.textFaint)
                    }
                }
            }
        }
        if (badge != null) {
            Box(
                Modifier.offset(x = 24.dp, y = (-11).dp).height(22.dp).clip(CircleShape).background(c.accent).padding(horizontal = 10.dp),
                contentAlignment = Alignment.Center,
            ) {
                Text(badge, style = PP.type.caption.copy(fontSize = 11.5.sp, lineHeight = 11.5.sp, fontWeight = FontWeight.SemiBold, letterSpacing = 0.02.em), color = c.onAccent)
            }
        }
    }
}

/** Credit level (css .pp-credit-chip--low / --empty, .pp-wallet--low). */
enum class PPCreditLevel { Normal, Low, Empty }

/** Header wallet chip (css .pp-credit-chip): coins icon, USD amount, optional + button to top up. */
@Composable
fun PPCreditChip(amount: String, modifier: Modifier = Modifier, level: PPCreditLevel = PPCreditLevel.Normal, onClick: () -> Unit = {}, onAdd: (() -> Unit)? = null) {
    val c = PP.colors
    val (bg, bd, fg, ic) = when (level) {
        PPCreditLevel.Normal -> listOf(c.surface, c.borderStrong, c.text, c.success)
        PPCreditLevel.Low -> listOf(c.warningFill, c.warningBorder, c.warning, c.warning)
        PPCreditLevel.Empty -> listOf(c.dangerFill, c.dangerBorder, c.danger, c.danger)
    }
    Row(
        modifier
            .height(34.dp)
            .clip(CircleShape)
            .background(bg)
            .border(1.dp, bd, CircleShape)
            .clickable(role = Role.Button, onClick = onClick)
            .semantics(mergeDescendants = true) { contentDescription = "Credit balance $amount" + if (level != PPCreditLevel.Normal) ", ${level.name.lowercase()}" else "" }
            .padding(start = 12.dp, end = if (onAdd != null) 6.dp else 12.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(8.dp),
    ) {
        PPIconView(PPIcon.Coins, size = 16.dp, tint = ic)
        Text(amount, style = PP.type.bodyS.copy(fontWeight = FontWeight.SemiBold, lineHeight = 13.5.sp, fontFeatureSettings = "tnum"), color = fg)
        if (onAdd != null) {
            Box(
                Modifier.size(24.dp).clip(CircleShape).background(c.accent).clickable(role = Role.Button, onClick = onAdd).semantics { contentDescription = "Add credit" },
                contentAlignment = Alignment.Center,
            ) { PPIconView(PPIcon.Plus, size = 14.dp, tint = c.onAccent) }
        }
    }
}

/**
 * Credit balance card (css .pp-wallet): "Credit balance" label, 40sp amount with currency, held
 * amount line and actions. [level] Low turns the edge and amount amber.
 */
@Composable
fun PPCreditBalance(
    amount: String,
    modifier: Modifier = Modifier,
    currency: String = "USD",
    label: String = "Credit balance",
    held: String? = null,
    level: PPCreditLevel = PPCreditLevel.Normal,
    actions: (@Composable RowScope.() -> Unit)? = null,
) {
    val c = PP.colors
    val shape = RoundedCornerShape(PPRadius.xl)
    val amountColor = when (level) {
        PPCreditLevel.Normal -> c.text
        PPCreditLevel.Low -> c.warning
        PPCreditLevel.Empty -> c.danger
    }
    Column(
        modifier
            .fillMaxWidth()
            .shadow(1.dp, shape)
            .clip(shape)
            .background(c.surface)
            .border(1.dp, if (level == PPCreditLevel.Normal) c.border else if (level == PPCreditLevel.Low) c.warningBorder else c.dangerBorder, shape)
            .padding(24.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Column(Modifier.semantics(mergeDescendants = true) {}) {
            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                PPIconView(PPIcon.Coins, size = 18.dp, tint = c.success)
                Text(label, style = PP.type.bodyS.copy(fontWeight = FontWeight.Medium), color = c.textMuted)
            }
            Text(
                buildAnnotatedString {
                    append(amount)
                    withStyle(SpanStyle(fontSize = 15.sp, fontWeight = FontWeight.Medium, color = c.textMuted, letterSpacing = 0.sp)) { append(" $currency") }
                },
                Modifier.padding(top = 6.dp),
                style = PP.type.displayM.copy(fontSize = 40.sp, lineHeight = 44.sp, letterSpacing = (-0.02).em, fontFeatureSettings = "tnum"),
                color = amountColor,
            )
            if (held != null) Text(held, style = PP.type.bodyS.copy(fontFeatureSettings = "tnum"), color = c.textMuted)
        }
        if (actions != null) Row(horizontalArrangement = Arrangement.spacedBy(8.dp), content = actions)
    }
}

/** Listing logo colours (css .pp-listing-logo--*). */
enum class PPLogoTone(val color: Color) {
    Navy(PPPalette.navy700), Blue(PPPalette.blue600), Violet(PPPalette.violet600), Teal(PPPalette.teal600),
    Amber(PPPalette.amber700), Green(PPPalette.green700), Red(PPPalette.red700),
}

/** Square marketplace logo: letters or an icon on a solid brand colour. */
@Composable
fun PPListingLogo(modifier: Modifier = Modifier, text: String? = null, icon: PPIcon? = null, tone: PPLogoTone = PPLogoTone.Navy, large: Boolean = false) {
    val size = if (large) 72.dp else 48.dp
    val shape = RoundedCornerShape(if (large) PPRadius.xl else PPRadius.l)
    Box(
        modifier.size(size).clip(shape).background(tone.color).border(1.dp, Color.White.copy(alpha = 0.12f), shape),
        contentAlignment = Alignment.Center,
    ) {
        if (icon != null) PPIconView(icon, size = if (large) 34.dp else 24.dp, tint = Color.White)
        else Text(text ?: "", style = PP.type.h3.copy(fontSize = if (large) 26.sp else 17.sp, lineHeight = if (large) 26.sp else 17.sp, fontWeight = FontWeight.Bold, letterSpacing = (-0.01).em), color = Color.White)
    }
}

/**
 * Marketplace listing (css .pp-listing-card): logo, title, publisher with verified mark, category
 * pill, three-line description, price + host footer. [comingSoon] adds the ribbon and fades the price.
 */
@Composable
fun PPListingCard(
    title: String,
    publisher: String,
    description: String,
    price: String,
    modifier: Modifier = Modifier,
    priceUnit: String? = null,
    category: String? = null,
    categoryIcon: PPIcon? = null,
    host: String? = null,
    verified: Boolean = false,
    logoText: String? = null,
    logoIcon: PPIcon? = null,
    logoTone: PPLogoTone = PPLogoTone.Navy,
    comingSoon: Boolean = false,
    onClick: () -> Unit = {},
) {
    val c = PP.colors
    val shape = RoundedCornerShape(PPRadius.xl)
    Box(modifier) {
        Column(
            Modifier
                .fillMaxWidth()
                .shadow(1.dp, shape)
                .clip(shape)
                .background(c.surface)
                .border(1.dp, c.border, shape)
                .clickable(role = Role.Button, onClickLabel = "Open $title", onClick = onClick)
                .padding(20.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            Row(Modifier.padding(end = if (comingSoon) 48.dp else 0.dp), verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
                PPListingLogo(text = logoText, icon = logoIcon, tone = logoTone)
                Column(verticalArrangement = Arrangement.spacedBy(3.dp)) {
                    Text(title, Modifier.semantics { heading() }, style = PP.type.h4, color = c.text, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(6.dp)) {
                        Text(publisher, style = PP.type.mono.copy(fontSize = 12.sp, lineHeight = 16.sp), color = c.textMuted)
                        if (verified) {
                            Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(3.dp)) {
                                PPIconView(PPIcon.BadgeCheck, size = 15.dp, tint = c.textLink)
                                Text("Verified", style = PP.type.caption.copy(fontWeight = FontWeight.Medium), color = c.textLink)
                            }
                        }
                    }
                }
            }
            if (category != null) {
                Row(
                    Modifier.height(24.dp).clip(CircleShape).background(c.surfaceSunken).border(1.dp, c.borderSubtle, CircleShape).padding(horizontal = 9.dp),
                    verticalAlignment = Alignment.CenterVertically,
                    horizontalArrangement = Arrangement.spacedBy(5.dp),
                ) {
                    if (categoryIcon != null) PPIconView(categoryIcon, size = 13.dp, tint = c.textMuted)
                    Text(category, style = PP.type.caption.copy(fontWeight = FontWeight.Medium, lineHeight = 12.sp), color = c.textMuted)
                }
            }
            Text(description, style = PP.type.bodyS, color = c.textBody, maxLines = 3, overflow = TextOverflow.Ellipsis)
            Box(Modifier.fillMaxWidth().height(1.dp).background(c.borderSubtle))
            Row(Modifier.fillMaxWidth(), horizontalArrangement = Arrangement.SpaceBetween, verticalAlignment = Alignment.Bottom) {
                Text(
                    buildAnnotatedString {
                        append(price)
                        if (priceUnit != null) withStyle(SpanStyle(fontWeight = FontWeight.Normal, color = c.textMuted, fontSize = PPType.caption)) { append(" $priceUnit") }
                    },
                    style = PP.type.body.copy(fontWeight = FontWeight.SemiBold, fontFeatureSettings = "tnum"),
                    color = if (comingSoon) c.textFaint else c.text,
                )
                if (host != null) {
                    Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(4.dp)) {
                        PPIconView(PPIcon.Server, size = 13.dp, tint = c.textFaint)
                        Text(host, style = PP.type.caption, color = c.textFaint)
                    }
                }
            }
        }
        if (comingSoon) {
            Text(
                "Coming soon",
                Modifier.align(Alignment.TopEnd).padding(12.dp).clip(CircleShape).background(c.infoFill).border(1.dp, c.infoBorder, CircleShape).padding(horizontal = 8.dp, vertical = 3.dp),
                style = PP.type.caption.copy(fontSize = 11.sp, fontWeight = FontWeight.SemiBold),
                color = c.info,
            )
        }
    }
}

/** Nest status (css .pp-nest-status[data-status]). */
enum class PPNestStatus { Running, Sleeping, Setup, Paused, Error }

/** Status pill: a dot and a word ("Running", "Sleeping", "Paused · hours used up"). */
@Composable
fun PPNestStatusPill(status: PPNestStatus, label: String, modifier: Modifier = Modifier) {
    val c = PP.colors
    val (dot, fg, bg, bd) = when (status) {
        PPNestStatus.Running -> listOf(c.successSolid, c.success, c.successFill, c.successBorder)
        PPNestStatus.Sleeping -> listOf(c.textFaint, c.textMuted, c.surfaceSunken, c.borderSubtle)
        PPNestStatus.Setup -> listOf(c.infoSolid, c.info, c.infoFill, c.infoBorder)
        PPNestStatus.Paused -> listOf(c.warningSolid, c.warning, c.warningFill, c.warningBorder)
        PPNestStatus.Error -> listOf(c.dangerSolid, c.danger, c.dangerFill, c.dangerBorder)
    }
    Row(
        modifier.height(24.dp).clip(CircleShape).background(bg).border(1.dp, bd, CircleShape).padding(start = 8.dp, end = 9.dp),
        verticalAlignment = Alignment.CenterVertically,
        horizontalArrangement = Arrangement.spacedBy(6.dp),
    ) {
        Box(
            Modifier.size(8.dp)
                .then(if (status == PPNestStatus.Running) Modifier.drawBehind { drawCircle(dot.copy(alpha = 0.25f), radius = size.minDimension / 2 + 3.dp.toPx()) } else Modifier)
                .clip(CircleShape).background(dot),
        )
        Text(label, style = PP.type.caption.copy(fontWeight = FontWeight.Medium, lineHeight = 12.sp), color = fg, maxLines = 1)
    }
}

/** One usage meter line on a Nest card. */
data class PPMeterData(val label: String, val value: String, val fraction: Float, val status: PPStatus = PPStatus.Info)

/**
 * Nest card (css .pp-nest-card): avatar (gradient bird; grey moon when sleeping; dashed while
 * setting up), name + handle, status pill, package line, usage meters, note and footer actions.
 */
@Composable
fun PPNestCard(
    name: String,
    handle: String,
    status: PPNestStatus,
    statusLabel: String,
    modifier: Modifier = Modifier,
    packageName: String? = null,
    packageDetail: String? = null,
    meters: List<PPMeterData> = emptyList(),
    note: String? = null,
    primaryLabel: String? = null,
    onPrimary: () -> Unit = {},
    onSettings: (() -> Unit)? = null,
) {
    val c = PP.colors
    val shape = RoundedCornerShape(PPRadius.xl)
    val dashColor = c.borderStrong
    Column(
        modifier
            .fillMaxWidth()
            .shadow(1.dp, shape)
            .clip(shape)
            .background(c.surface)
            .border(1.dp, c.border, shape)
            .padding(20.dp),
        verticalArrangement = Arrangement.spacedBy(16.dp),
    ) {
        Row(verticalAlignment = Alignment.CenterVertically, horizontalArrangement = Arrangement.spacedBy(12.dp)) {
            val avatarBg = when (status) {
                PPNestStatus.Sleeping -> Modifier.background(Brush.linearGradient(listOf(PPPalette.gray400, PPPalette.gray600), Offset.Zero, Offset(44f * 2, 44f * 2.6f)))
                PPNestStatus.Setup -> Modifier.background(c.surfaceSunken).drawBehind {
                    drawCircle(dashColor, style = Stroke(1.dp.toPx(), pathEffect = PathEffect.dashPathEffect(floatArrayOf(4.dp.toPx(), 3.dp.toPx()))))
                }
                else -> Modifier.background(Brush.linearGradient(listOf(PPPalette.blue500, PPPalette.navy700), Offset.Zero, Offset(44f * 2, 44f * 2.6f)))
            }
            Box(Modifier.size(44.dp).clip(CircleShape).then(avatarBg), contentAlignment = Alignment.Center) {
                PPIconView(
                    when (status) { PPNestStatus.Sleeping -> PPIcon.Moon; PPNestStatus.Setup -> PPIcon.LoaderCircle; else -> PPIcon.Bird },
                    size = 22.dp, tint = if (status == PPNestStatus.Setup) c.textMuted else Color.White,
                )
            }
            Column(Modifier.weight(1f).semantics(mergeDescendants = true) { heading() }, verticalArrangement = Arrangement.spacedBy(2.dp)) {
                Text(name, style = PP.type.body.copy(fontWeight = FontWeight.SemiBold), color = c.text, maxLines = 1, overflow = TextOverflow.Ellipsis)
                Text(handle, style = PP.type.mono.copy(fontSize = 12.5.sp, lineHeight = 16.sp), color = c.textMuted, maxLines = 1)
            }
            PPNestStatusPill(status, statusLabel)
        }
        if (packageName != null) {
            Row(
                Modifier.fillMaxWidth().clip(RoundedCornerShape(PPRadius.m)).background(c.surfaceSunken).padding(horizontal = 12.dp, vertical = 10.dp),
                horizontalArrangement = Arrangement.SpaceBetween,
            ) {
                Text(packageName, style = PP.type.bodyS.copy(fontWeight = FontWeight.SemiBold), color = c.text)
                if (packageDetail != null) Text(packageDetail, style = PP.type.bodyS, color = c.textMuted)
            }
        }
        if (meters.isNotEmpty()) {
            Column(verticalArrangement = Arrangement.spacedBy(12.dp)) {
                meters.forEach { PPMeter(it.label, it.value, it.fraction, status = it.status) }
            }
        }
        if (note != null) Text(note, style = PP.type.bodyS, color = c.textMuted)
        if (primaryLabel != null || onSettings != null) {
            Row(horizontalArrangement = Arrangement.spacedBy(8.dp), verticalAlignment = Alignment.CenterVertically) {
                if (primaryLabel != null) PPButton(primaryLabel, onPrimary, Modifier.weight(1f), leadingIcon = PPIcon.Inbox, fullWidth = true)
                if (onSettings != null) PPIconButton(PPIcon.Settings, "Nest settings", onSettings, variant = PPIconButtonVariant.Default, size = PPIconButtonSize.M)
            }
        }
    }
}

@PPPreviews
@Composable
private fun PricingPreview() = PPPreviewSurface {
    PPPricingCard(
        "Pro", "$12", "per month",
        listOf(PPFeature("5 addresses"), PPFeature("Meeting notes and summaries"), PPFeature("Calls with up to 30 people"), PPFeature("Custom domain", included = false)),
        "Upgrade to Pro", {},
        description = "For people who work with agents every day.", note = "Billed yearly: $120", icon = PPIcon.Sparkles,
        featured = true, badge = "Most popular",
    )
    PPPricingCard("Free", "$0", "forever", listOf(PPFeature("1 address"), PPFeature("Calls with up to 4 people"), PPFeature("Meeting summaries", included = false)), "Choose Free", {}, current = true)
}

@PPPreviews
@Composable
private fun CreditPreview() = PPPreviewSurface {
    Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
        PPCreditChip("$24.50", onAdd = {})
        PPCreditChip("$1.20", level = PPCreditLevel.Low)
        PPCreditChip("$0.00", level = PPCreditLevel.Empty, onAdd = {})
    }
    PPCreditBalance("$24.50", held = "$3.00 held for 2 running jobs") {
        PPButton("Top up", {}, leadingIcon = PPIcon.Plus)
        PPButton("History", {}, variant = PPButtonVariant.Default, leadingIcon = PPIcon.History)
    }
    PPCreditBalance("$1.20", level = PPCreditLevel.Low)
}

@PPPreviews
@Composable
private fun MarketplacePreview() = PPPreviewSurface {
    PPListingCard(
        "Contract Review", "/lexa",
        "Send a contract as an attachment; get back the risky clauses, what they mean and a suggested change, in English or Turkish.",
        "$25.00", priceUnit = "per run", category = "Legal", categoryIcon = PPIcon.Briefcase, host = "Hosted by the publisher",
        verified = true, logoText = "Lx", logoTone = PPLogoTone.Violet,
    )
    PPListingCard(
        "Translate to Turkish", "/dil", "Plain, natural Turkish for emails, contracts and product copy.", "$0.02",
        priceUnit = "per 100 words", category = "Language", categoryIcon = PPIcon.Languages, logoIcon = PPIcon.Languages, logoTone = PPLogoTone.Teal, comingSoon = true,
    )
}

@PPPreviews
@Composable
private fun NestPreview() = PPPreviewSurface {
    PPNestCard(
        "Ada's assistant", "/ada/nest", PPNestStatus.Running, "Running",
        packageName = "Pigeonpost AI", packageDetail = "$10 / month · renews 2 Dec",
        meters = listOf(
            PPMeterData("Running hours", "6.2 of 10 h", 0.62f),
            PPMeterData("Input tokens", "410K of 1M", 0.41f),
            PPMeterData("Output tokens", "88K of 100K", 0.88f, PPStatus.Warning),
        ),
        primaryLabel = "Open in inbox", onSettings = {},
    )
    PPNestCard("HR helper", "/wodo/hr", PPNestStatus.Sleeping, "Sleeping", note = "Wakes when a message arrives. Sleeping does not use running hours.")
    PPNestCard("Research Nest", "/emre/research", PPNestStatus.Setup, "Setting up", note = "Preparing the sandbox. This takes about a minute.")
}
