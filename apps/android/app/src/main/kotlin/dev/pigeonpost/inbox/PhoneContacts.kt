package dev.pigeonpost.inbox

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.provider.ContactsContract.CommonDataKinds.Phone
import android.telephony.PhoneNumberUtils
import android.telephony.TelephonyManager
import androidx.core.content.ContextCompat
import dev.pigeonpost.core.contacts.*
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ensureActive
import kotlinx.coroutines.withContext
import org.json.JSONObject
import java.security.MessageDigest
import java.util.Locale

object PhoneContacts {
    fun defaultRegion(context: Context): String {
        val phone = context.getSystemService(TelephonyManager::class.java)
        return sequenceOf(runCatching { phone?.networkCountryIso }.getOrNull(), runCatching { phone?.simCountryIso }.getOrNull(), Locale.getDefault().country)
            .filterNotNull().map { it.uppercase(Locale.ROOT) }.firstOrNull { it.matches(Regex("[A-Z]{2}")) } ?: "US"
    }
    fun normalize(raw: String, region: String): String? {
        if (!region.matches(Regex("[A-Za-z]{2}")) || raw.length > 128) return null
        val number = raw.trim().removePrefix("tel:").split(Regex(";ext=|[xX](?=[0-9])"), limit = 2)[0]
        if (number.any { it.isLetter() }) return null
        val formatted = PhoneNumberUtils.formatNumberToE164(number, region.uppercase(Locale.ROOT)) ?: return null
        return formatted.takeIf(PeopleInput::phone)
    }
    /** Invoked only after the disclosure and READ_CONTACTS grant, never in a background job. */
    suspend fun read(context: Context, region: String): List<DeviceContact> = withContext(Dispatchers.IO) {
        check(ContextCompat.checkSelfPermission(context, Manifest.permission.READ_CONTACTS) == PackageManager.PERMISSION_GRANTED)
        val result = linkedMapOf<String, DeviceContact>()
        context.contentResolver.query(Phone.CONTENT_URI, arrayOf(Phone.NUMBER, Phone.NORMALIZED_NUMBER, Phone.DISPLAY_NAME), null, null, null)?.use { rows ->
            val raw = rows.getColumnIndexOrThrow(Phone.NUMBER); val normalized = rows.getColumnIndexOrThrow(Phone.NORMALIZED_NUMBER)
            val name = rows.getColumnIndexOrThrow(Phone.DISPLAY_NAME)
            while (rows.moveToNext() && result.size < 5000) {
                ensureActive()
                val number = rows.getString(normalized)?.takeIf(PeopleInput::phone) ?: normalize(rows.getString(raw).orEmpty(), region) ?: continue
                val label = rows.getString(name).orEmpty().filterNot(Char::isISOControl).take(128).ifBlank { number }
                result.putIfAbsent(number, DeviceContact(number, label))
            }
        }
        result.values.toList()
    }
}

/** Existing app-private preference storage; aliases never enter the postbox contact table. */
class AndroidLocalAliases(context: Context, fixtures: Boolean) : LocalAliases {
    private val prefs = context.getSharedPreferences(if (fixtures) "fixture-local-aliases" else "local-aliases-v1", Context.MODE_PRIVATE)
    private fun key(subject: String, mailbox: String) = MessageDigest.getInstance("SHA-256").digest((subject + "\n" + mailbox).toByteArray())
        .joinToString("") { "%02x".format(it.toInt() and 255) }
    override fun read(subject: String, mailbox: String): Map<String, String> = runCatching {
        val json = JSONObject(prefs.getString(key(subject, mailbox), "{}").orEmpty())
        json.keys().asSequence().take(5000).associateWith { json.getString(it) }
    }.getOrDefault(emptyMap())
    override fun write(subject: String, mailbox: String, values: Map<String, String>) {
        check(prefs.edit().putString(key(subject, mailbox), JSONObject(values).toString()).commit()) { "Could not save the local name." }
    }
    override fun clear() { check(prefs.edit().clear().commit()) { "Could not clear local names." } }
    override fun namePromptSeen(subject: String, mailbox: String) = prefs.getBoolean("prompt:" + key(subject, mailbox), false)
    override fun namePromptSeen(subject: String, mailbox: String, seen: Boolean) {
        check(prefs.edit().putBoolean("prompt:" + key(subject, mailbox), seen).commit()) { "Could not save the prompt preference." }
    }
}
