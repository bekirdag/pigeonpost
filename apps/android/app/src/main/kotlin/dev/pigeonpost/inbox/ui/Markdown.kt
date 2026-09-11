package dev.pigeonpost.inbox.ui

import androidx.compose.foundation.horizontalScroll
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.text.ClickableText
import androidx.compose.foundation.text.selection.SelectionContainer
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.AnnotatedString
import androidx.compose.ui.text.SpanStyle
import androidx.compose.ui.text.buildAnnotatedString
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.dp
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import org.commonmark.node.*
import org.commonmark.parser.Parser

private data class Block(val text: AnnotatedString, val kind: String = "text", val depth: Int = 0)
private val parser = Parser.builder().build()

/** CommonMark becomes native text. HTML stays text; remote images never load automatically. */
@Suppress("DEPRECATION")
@Composable
fun Markdown(body: String, openLink: (String) -> Unit) {
    val blocks by produceState<List<Block>?>(null, body) {
        value = withContext(Dispatchers.Default) { markdownBlocks(body.take(128 * 1024)) }
    }
    SelectionContainer {
        Column {
            val content = blocks
            if (content == null) Text(body.take(500), style = MaterialTheme.typography.bodyLarge)
            else content.forEach { block ->
                when (block.kind) {
                    "rule" -> HorizontalDivider(Modifier.padding(vertical = 8.dp))
                    "code" -> Surface(color = MaterialTheme.colorScheme.onSurface.copy(alpha = .05f), shape = MaterialTheme.shapes.small, modifier = Modifier.fillMaxWidth().padding(vertical = 4.dp)) {
                        Text(block.text, fontFamily = FontFamily.Monospace, style = MaterialTheme.typography.bodyMedium,
                            modifier = Modifier.horizontalScroll(rememberScrollState()).padding(10.dp))
                    }
                    else -> ClickableText(block.text,
                        style = (if (block.kind == "heading") MaterialTheme.typography.titleMedium else MaterialTheme.typography.bodyLarge)
                            .copy(color = MaterialTheme.colorScheme.onSurface),
                        modifier = Modifier.padding(start = (block.depth * 12).dp, bottom = 6.dp),
                        onClick = { offset -> block.text.getStringAnnotations("url", offset, offset).firstOrNull()?.let { openLink(it.item) } })
                }
            }
            if (body.length > 128 * 1024) Text("Use Original message to read the complete text.", style = MaterialTheme.typography.labelMedium)
        }
    }
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
                    is Code -> { pushStyle(SpanStyle(fontFamily = FontFamily.Monospace)); append(token.literal); pop() }
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
