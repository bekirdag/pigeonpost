package dev.pigeonpost.core.calendar

import java.net.URI
import java.net.URLDecoder
import java.nio.charset.StandardCharsets

/** Public meeting locators and personal browser invitations never grant member authority. */
sealed interface CalendarLink {
    data class Member(val code: String) : CalendarLink
    class Guest(val url: String) : CalendarLink { override fun toString() = "CalendarLink.Guest()" }
    companion object {
        /** Existing six-character ad-hoc links keep their browser flow instead of looping into this app. */
        fun browserOnly(input: String): String? = try {
            val uri = URI(input)
            if (input.length <= 4096 && uri.scheme.equals("https", true) && uri.host.equals("inbox.pigeonpost.dev", true) &&
                uri.rawUserInfo == null && uri.port == -1 && uri.rawQuery == null && uri.rawFragment == null &&
                Regex("/m/[A-Za-z0-9]{6}").matches(uri.rawPath)) "https://inbox.pigeonpost.dev" + uri.rawPath else null
        } catch (_: Exception) { null }
        fun parse(input: String): CalendarLink? {
            if (input.toByteArray().size > 4096) return null
            return try {
                val uri = URI(input)
                if (uri.rawUserInfo != null || uri.port != -1 || uri.rawQuery != null) return null
                val locator = when {
                    uri.scheme.equals("https", true) && uri.host.equals("inbox.pigeonpost.dev", true) && uri.rawPath.startsWith("/m/") -> uri.rawPath.removePrefix("/m/")
                    uri.scheme == "dev.pigeonpost.inbox" && uri.host == "m" && uri.rawPath.startsWith('/') -> uri.rawPath.removePrefix("/")
                    else -> return null
                }
                val code = CalendarMeetingCode.parse(locator)
                val fragment = uri.rawFragment
                if (fragment.isNullOrEmpty()) return code?.let(::Member)
                if (fragment.toByteArray().size > 2300) return null
                val pairs = fragment.split('&').map { part ->
                    val fields = part.split('=', limit = 2)
                    if (fields.size != 2) return null
                    URLDecoder.decode(fields[0], StandardCharsets.UTF_8.name()) to URLDecoder.decode(fields[1], StandardCharsets.UTF_8.name())
                }
                if (pairs.map { it.first }.distinct().size != pairs.size) return null
                val fields = pairs.toMap()
                fun token(value: String?) = value != null && Regex("[A-Za-z0-9_-]{1,2048}").matches(value)
                if (locator == "u") {
                    if (fields.size != 1 || !token(fields["u"])) return null
                } else {
                    if (code == null && !Regex("evt_[A-Za-z0-9_-]{26}").matches(locator)) return null
                    if (fields.size !in 1..2 || fields.keys.any { it !in setOf("t", "r") } || !token(fields["t"]) ||
                        fields["r"]?.let { it !in setOf("accepted", "tentative", "declined") } == true) return null
                }
                Guest("https://inbox.pigeonpost.dev/m/$locator#$fragment")
            } catch (_: Exception) { null }
        }
    }
}
