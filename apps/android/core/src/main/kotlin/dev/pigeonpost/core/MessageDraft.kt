package dev.pigeonpost.core

import kotlinx.serialization.json.JsonObject
import kotlinx.serialization.json.JsonPrimitive
import kotlinx.serialization.json.contentOrNull
import kotlinx.serialization.json.intOrNull
import java.util.UUID

enum class MessageComposerKind { REPLY, EDIT }

data class MessageComposerAction(
    val kind: MessageComposerKind,
    val messageId: String,
    val reference: String,
    val author: String,
    val quote: String,
    val originalBody: String,
    val previousDraft: String? = null,
    val id: String = UUID.randomUUID().toString(),
) {
    val editing get() = kind == MessageComposerKind.EDIT
}

data class MessageInfoState(val messageId: String, val info: MessageInfo? = null, val loading: Boolean = true, val error: String? = null)

fun ThreadMessage.canEditMessage(features: Set<String>, now: Long): Boolean =
    Features.MESSAGE_EDIT in features && outgoing && !system && status == Delivery.SENT &&
        at > 0 && now >= at && now - at <= 86_400 && (expiresAt == null || expiresAt > now) && pendingEditId == null

fun replyQuote(text: String): String {
    val line = text.trim().lineSequence().firstOrNull().orEmpty()
    return line.substring(0, line.offsetByCodePoints(0, minOf(200, line.codePointCount(0, line.length))))
}

/** Edit the displayed text slot; retain the request's verb and unrelated structured arguments. */
fun editedMessageBody(original: String, text: String): String {
    val root = runCatching { WireJson.parseToJsonElement(original) as? JsonObject }.getOrNull() ?: return text
    if ((root["v"] as? JsonPrimitive)?.intOrNull != 1) return text
    val display = displayBody(original, outgoing = true)
    if (display.requestVerb == null) return text
    val args = root["args"] as? JsonObject
    val field = when {
        (args?.get("task") as? JsonPrimitive)?.contentOrNull != null -> "task"
        (args?.get("question") as? JsonPrimitive)?.contentOrNull != null -> "question"
        else -> null
    }
    val changed = root.toMutableMap()
    if (field != null && args != null) changed["args"] = JsonObject(args + (field to JsonPrimitive(text)))
    if (field == null || (root["note"] as? JsonPrimitive)?.contentOrNull == display.text) changed["note"] = JsonPrimitive(text)
    return JsonObject(changed).toString()
}
