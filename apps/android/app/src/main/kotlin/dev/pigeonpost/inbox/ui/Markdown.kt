package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.background
import androidx.compose.foundation.border
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.shape.RoundedCornerShape
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.LinkAnnotation
import androidx.compose.ui.text.TextLinkStyles
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.unit.sp
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.commonmark.node.*
import org.commonmark.parser.Parser

private data class Block(val text: AnnotatedString, val kind: String = "text", val depth: Int = 0)
private val parser = Parser.builder().build()

/**
 * CommonMark becomes native text. HTML stays text; remote images never load automatically.
 * The whole body is one selection, so part of a message can be selected and copied; links stay
 * tappable inside it.
 */
@Composable
fun Markdown(body: String, openLink: (String) -> Unit, mine: Boolean = false, held: Boolean = false) {
    val p = Pp.colors
    val ink = if (mine) p.mineText else if (held) p.heldCardTitle else p.body
    val strong = if (mine) p.mineText else if (held) p.heldCardTitle else p.ink
    val panel = if (mine) p.mineRule else if (held) p.heldPillFill else p.codeFill
    val panelStroke = if (mine) p.mineStroke else if (held) p.heldPillStroke else p.codeStroke
    val currentOpen by rememberUpdatedState(openLink)
    val blocks by produceState<List<Block>?>(null, body) {
        value = withContext(Dispatchers.Default) { markdownBlocks(body.take(128 * 1024)) }
    }
    SelectionContainer {
        Column {
            val content = blocks
            if (content == null) Text(body.take(500), style = geist(15.sp, color = ink).copy(lineHeight = 22.sp))
            else content.forEach { block ->
                when (block.kind) {
                    "rule" -> HorizontalDivider(Modifier.padding(vertical = 8.dp), color = panelStroke)
                    "code" -> Box(Modifier.fillMaxWidth().padding(vertical = 4.dp).clip(RoundedCornerShape(10.dp)).background(panel)
                        .border(1.dp, panelStroke, RoundedCornerShape(10.dp))) {
                        Text(block.text, style = geistMono(13.sp, color = if (mine) p.mineText else p.codeText).copy(lineHeight = 19.sp),
                            modifier = Modifier.horizontalScroll(rememberScrollState()).padding(horizontal = 12.dp, vertical = 10.dp))
                    }
                    else -> {
                        val linked = remember(block.text, p) { withLinks(block.text, if (mine) p.accent else p.accent, panel) { currentOpen(it) } }
                        Text(linked,
                            style = (if (block.kind == "heading") geist(16.5.sp, FontWeight.SemiBold, strong).copy(lineHeight = 23.sp)
                                else geist(15.sp, color = ink).copy(lineHeight = 22.sp)),
                            modifier = Modifier.padding(start = (block.depth * 12).dp, bottom = 6.dp))
                    }
                }
            }
            if (body.length > 128 * 1024) Text("Use Original message to read the complete text.", style = geist(12.sp), color = p.muted)
        }
    }
}

/** Links become real link annotations (tappable inside a selection); inline code gets the mono panel. */
private fun withLinks(text: AnnotatedString, link: Color, codeFill: Color, open: (String) -> Unit): AnnotatedString = buildAnnotatedString {
    append(text)
    text.getStringAnnotations("url", 0, text.length).forEach { range ->
        addLink(LinkAnnotation.Clickable(range.item, TextLinkStyles(SpanStyle(color = link, textDecoration = TextDecoration.Underline))) { open(range.item) },
            range.start, range.end)
    }
    text.getStringAnnotations("code", 0, text.length).forEach { range -> addStyle(SpanStyle(background = codeFill), range.start, range.end) }
}

private fun markdownBlocks(text: String): List<Block> {
    val output = mutableListOf<Block>()
    fun inline(node: Node, prefix: String = ""): AnnotatedString = buildAnnotatedString {
        append(prefix)
        fun visit(parent: Node) {
            var child = parent.firstChild
            while (child != null) {
                val token = child
                when (token) {
                    is org.commonmark.node.Text -> append(token.literal)
                    is Code -> { pushStringAnnotation("code", ""); pushStyle(SpanStyle(fontFamily = GeistMono, fontSize = 13.5.sp)); append(token.literal); pop(); pop() }
                    is StrongEmphasis -> { pushStyle(SpanStyle(fontWeight = FontWeight.Bold)); visit(token); pop() }
                    is Emphasis -> { pushStyle(SpanStyle(fontStyle = FontStyle.Italic)); visit(token); pop() }
                    is Link -> {
                        pushStringAnnotation("url", token.destination)
                        pushStyle(SpanStyle(textDecoration = TextDecoration.Underline)); visit(token); pop(); pop()
                    }
                    is Image -> { append("[Image: "); visit(token); append("]") }
                    is SoftLineBreak -> append(" ")
                    is HardLineBreak -> append("\n")
                    is HtmlInline -> append(token.literal)
                    else -> visit(token)
                }
                child = token.next
            }
        }
        visit(node)
    }
    fun walk(parent: Node, depth: Int = 0, prefix: String = "") {
        var child = parent.firstChild
        var index = (parent as? OrderedList)?.markerStartNumber ?: 1
        while (child != null) {
            val node = child
            when (node) {
                is Paragraph -> output += Block(inline(node, prefix), depth = depth)
                is Heading -> output += Block(inline(node, prefix), "heading", depth)
                is FencedCodeBlock -> output += Block(AnnotatedString(node.literal.trimEnd()), "code", depth)
                is IndentedCodeBlock -> output += Block(AnnotatedString(node.literal.trimEnd()), "code", depth)
                is ThematicBreak -> output += Block(AnnotatedString(""), "rule")
                is BlockQuote -> walk(node, depth + 1)
                is ListItem -> walk(node, depth, if (parent is OrderedList) "${index++}. " else "• ")
                is BulletList, is OrderedList -> walk(node, depth + 1)
                is HtmlBlock -> output += Block(AnnotatedString(node.literal), "code", depth)
                else -> walk(node, depth)
            }
            child = node.next
        }
    }
    walk(parser.parse(text)); return output
}
