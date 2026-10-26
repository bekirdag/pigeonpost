package dev.pigeonpost.core

import kotlinx.serialization.json.*
import java.util.Locale

/** Authentication requirements may change; the trusted issuer and redirect never do. */
class StrongSignInRequirement private constructor(val requestedACR: String, val maxAge: Int) {
    val acrValues: List<String> get() = requestedACR.split(' ')
    override fun toString() = "StrongSignInRequirement(methods=${acrValues.size}, maxAge=$maxAge)"

    companion object {
        fun create(acrValues: List<String>, maxAge: Int): StrongSignInRequirement? {
            if (maxAge !in 0..600 || acrValues.isEmpty() || acrValues.size > 8 || acrValues.distinct().size != acrValues.size ||
                acrValues.any { !Regex("[A-Za-z0-9:/._-]{1,128}").matches(it) }) return null
            return StrongSignInRequirement(acrValues.joinToString(" "), maxAge)
        }

        internal fun signalsChallenge(header: String?): Boolean = header != null &&
            (header.length > 2048 || header.contains("insufficient_user_authentication", ignoreCase = true))

        fun from(problem: JsonObject?, header: String?): StrongSignInRequirement? {
            val parameters = bearerParameters(header)
            val headerAge = parameters?.get("max_age")?.takeIf { Regex("[0-9]{1,3}").matches(it) }?.toIntOrNull()
            if (headerAge != null) create(parameters?.get("acr_values")?.split(' ')?.filter(String::isNotEmpty).orEmpty(), headerAge)?.let { return it }
            if (problem == null) return null
            val raw = problem["acr_values"]
            val values = when (raw) {
                is JsonArray -> raw.map { (it as? JsonPrimitive)?.takeIf(JsonPrimitive::isString)?.content ?: return null }
                is JsonPrimitive -> if (raw.isString) raw.content.split(' ').filter(String::isNotEmpty) else return null
                else -> return null
            }
            val age = (problem["max_age"] as? JsonPrimitive)?.takeUnless(JsonPrimitive::isString)?.intOrNull ?: return null
            return create(values, age)
        }

        /** Split parameters only outside quotes; descriptions cannot smuggle another parameter. */
        private fun bearerParameters(header: String?): Map<String, String>? {
            if (header == null || header.length > 2048 || !header.startsWith("Bearer ", ignoreCase = true) ||
                header.any { it.code < 32 || it.code == 127 }) return null
            val fields = header.substring(7)
            var index = 0
            val result = mutableMapOf<String, String>()
            while (index < fields.length) {
                while (index < fields.length && fields[index] == ' ') index++
                val start = index
                while (index < fields.length && (fields[index].isLetterOrDigit() && fields[index].code < 128 || fields[index] in "_-")) index++
                if (index == start) return null
                val name = fields.substring(start, index).lowercase(Locale.ROOT)
                while (index < fields.length && fields[index] == ' ') index++
                if (index >= fields.length || fields[index++] != '=') return null
                while (index < fields.length && fields[index] == ' ') index++
                if (index >= fields.length) return null
                val value = StringBuilder()
                if (fields[index] == '"') {
                    index++
                    var closed = false
                    while (index < fields.length) {
                        val character = fields[index++]
                        if (character == '"') { closed = true; break }
                        if (character == '\\') {
                            if (index >= fields.length || fields[index] !in "\\\"") return null
                            value.append(fields[index++])
                        } else value.append(character)
                    }
                    if (!closed) return null
                } else {
                    val begin = index
                    while (index < fields.length && fields[index] != ',' && fields[index] != ' ') {
                        if (fields[index] in "\"\\" || fields[index].code > 126) return null
                        value.append(fields[index++])
                    }
                    if (index == begin) return null
                }
                if (result.put(name, value.toString()) != null) return null
                while (index < fields.length && fields[index] == ' ') index++
                if (index == fields.length) return result
                if (fields[index++] != ',' || index == fields.length) return null
            }
            return result.takeIf { it.isNotEmpty() }
        }
    }
}
