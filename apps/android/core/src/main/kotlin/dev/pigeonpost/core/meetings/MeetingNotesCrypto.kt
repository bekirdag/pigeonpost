package dev.pigeonpost.core.meetings

import java.math.BigInteger
import java.security.AlgorithmParameters
import java.security.KeyFactory
import java.security.KeyPairGenerator
import java.security.SecureRandom
import java.security.interfaces.ECPrivateKey
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec
import java.security.spec.ECParameterSpec
import java.security.spec.ECPoint
import java.security.spec.ECPublicKeySpec
import java.util.Base64
import javax.crypto.Cipher
import javax.crypto.KeyAgreement
import javax.crypto.Mac
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

/**
 * This device's notes key: a P-256 key whose private half never leaves where it is kept (on Android,
 * the Keystore). Only key agreement is asked of it.
 */
interface NotesKey {
    /** `nk…`, 8–64 of `[A-Za-z0-9_-]`, never reused for a different key. */
    val keyId: String
    /** The public half: a 65-byte uncompressed P-256 point, standard base64. */
    val publicKey: String
    /** ECDH with a peer's uncompressed P-256 point: the 32-byte shared secret. */
    fun agree(peer: ECPublicKey): ByteArray
}

class MeetingNotesError(val code: String, message: String) : Exception(message)

/**
 * The meeting-notes cryptography of docs/meeting-summaries.md §3, as `site-inbox/calls-summary.js`
 * (open) and `meeting-intel/src/crypto.js` (seal) do it: P-256 ECDH → HKDF-SHA-256 → AES-256-GCM with
 * the 16-byte tag after the ciphertext, binary values in standard base64.
 */
object MeetingNotesCrypto {
    const val NOTES_INFO = "pigeonpost meeting notes v1|"
    private val random = SecureRandom()
    private val keyId = Regex("[A-Za-z0-9_-]{8,64}")
    val MEETING_ID = Regex("mtg_[a-z0-9]{26}")

    fun validKeyId(id: String) = keyId.matches(id)

    /** A fresh key id in the web's own form: `nk` + 12 random bytes, base64url without padding. */
    fun newKeyId(): String = "nk" + Base64.getUrlEncoder().withoutPadding().encodeToString(ByteArray(12).also(random::nextBytes))

    fun b64(bytes: ByteArray): String = Base64.getEncoder().encodeToString(bytes)
    fun unb64(text: String?, length: Int? = null): ByteArray {
        val value = text ?: throw MeetingNotesError("bad_ciphertext", "Missing value.")
        if (!Regex("[A-Za-z0-9+/]*={0,2}").matches(value)) throw MeetingNotesError("bad_ciphertext", "Not base64.")
        val bytes = try { Base64.getDecoder().decode(value) } catch (_: IllegalArgumentException) { throw MeetingNotesError("bad_ciphertext", "Not base64.") }
        if (length != null && bytes.size != length) throw MeetingNotesError("bad_ciphertext", "Unexpected length.")
        return bytes
    }

    /** The named curve's parameters (secp256r1), from the platform. */
    val p256: ECParameterSpec by lazy {
        AlgorithmParameters.getInstance("EC").apply { init(ECGenParameterSpec("secp256r1")) }.getParameterSpec(ECParameterSpec::class.java)
    }

    /** 0x04 ‖ X ‖ Y, each 32 bytes. */
    fun rawPublic(key: ECPublicKey): ByteArray {
        fun fixed(n: BigInteger): ByteArray {
            val bytes = n.toByteArray()
            return when {
                bytes.size == 32 -> bytes
                bytes.size == 33 && bytes[0] == 0.toByte() -> bytes.copyOfRange(1, 33)
                bytes.size < 32 -> ByteArray(32 - bytes.size) + bytes
                else -> throw IllegalArgumentException("Not a P-256 coordinate.")
            }
        }
        return byteArrayOf(4) + fixed(key.w.affineX) + fixed(key.w.affineY)
    }

    /** An uncompressed point, checked to lie on P-256 (the platform refuses points off the curve). */
    fun publicFromRaw(raw: ByteArray): ECPublicKey {
        if (raw.size != 65 || raw[0] != 4.toByte()) throw MeetingNotesError("bad_ciphertext", "Not an uncompressed P-256 point.")
        val point = ECPoint(BigInteger(1, raw.copyOfRange(1, 33)), BigInteger(1, raw.copyOfRange(33, 65)))
        val curve = p256.curve
        val p = (curve.field as java.security.spec.ECFieldFp).p
        val x = point.affineX; val y = point.affineY
        if (x >= p || y >= p || y.multiply(y).subtract(x.pow(3).add(curve.a.multiply(x)).add(curve.b)).mod(p) != BigInteger.ZERO)
            throw MeetingNotesError("bad_ciphertext", "The point is not on P-256.")
        return KeyFactory.getInstance("EC").generatePublic(ECPublicKeySpec(point, p256)) as ECPublicKey
    }

    /** HKDF-SHA-256 (RFC 5869) to 32 bytes, with the UTF-8 salt and info the web uses. */
    fun hkdf(secret: ByteArray, salt: String, info: String): ByteArray {
        val extract = Mac.getInstance("HmacSHA256").apply { init(SecretKeySpec(salt.toByteArray(Charsets.UTF_8).let { if (it.isEmpty()) ByteArray(32) else it }, "HmacSHA256")) }
        val prk = extract.doFinal(secret)
        val expand = Mac.getInstance("HmacSHA256").apply { init(SecretKeySpec(prk, "HmacSHA256")) }
        expand.update(info.toByteArray(Charsets.UTF_8)); expand.update(1)
        val okm = expand.doFinal()
        prk.fill(0)
        return okm
    }

    fun gcmOpen(key: ByteArray, iv: ByteArray, data: ByteArray, aad: String): ByteArray {
        if (data.size < 17) throw MeetingNotesError("bad_ciphertext", "Too short.")
        return try {
            Cipher.getInstance("AES/GCM/NoPadding").run {
                init(Cipher.DECRYPT_MODE, SecretKeySpec(key, "AES"), GCMParameterSpec(128, iv))
                updateAAD(aad.toByteArray(Charsets.UTF_8))
                doFinal(data)
            }
        } catch (_: java.security.GeneralSecurityException) { throw MeetingNotesError("bad_ciphertext", "The notes did not decrypt.") }
    }

    fun gcmSeal(key: ByteArray, plain: ByteArray, aad: String, iv: ByteArray = ByteArray(12).also(random::nextBytes)): Pair<ByteArray, ByteArray> =
        iv to Cipher.getInstance("AES/GCM/NoPadding").run {
            init(Cipher.ENCRYPT_MODE, SecretKeySpec(key, "AES"), GCMParameterSpec(128, iv))
            updateAAD(aad.toByteArray(Charsets.UTF_8))
            doFinal(plain)
        }

    /**
     * Open a ready meeting (`GET /v1/meetings/{id}`) with this device's key: unwrap the content key from
     * the wrap made for [key], then decrypt the brief. Only the meeting's own id binds both layers.
     */
    fun open(meeting: MeetingDetail, key: NotesKey): ByteArray {
        if (!MEETING_ID.matches(meeting.id)) throw MeetingNotesError("bad_ciphertext", "Unexpected meeting.")
        if (meeting.alg != null && meeting.alg != "A256GCM") throw MeetingNotesError("bad_ciphertext", "Unsupported notes.")
        val wrap = meeting.keys.firstOrNull { it.keyId == key.keyId } ?: throw MeetingNotesError("no_key_for_device", "These notes were encrypted for your other devices.")
        val shared = key.agree(publicFromRaw(unb64(wrap.epk, 65)))
        val kek = hkdf(shared, meeting.id, NOTES_INFO + wrap.keyId)
        shared.fill(0)
        val cek = try { gcmOpen(kek, unb64(wrap.iv, 12), unb64(wrap.wrapped, 48), "${meeting.id}|${wrap.keyId}") } finally { kek.fill(0) }
        try { return gcmOpen(cek, unb64(meeting.iv, 12), unb64(meeting.ciphertext), meeting.id) } finally { cek.fill(0) }
    }

    /**
     * What meeting-intel does (`sealBrief`): for tests and the on-device fixture only. Returns the
     * meeting's `alg/iv/ciphertext` and one wrap per recipient key.
     */
    fun seal(meetingId: String, plaintext: ByteArray, recipients: List<Pair<String, ByteArray>>): Sealed {
        val cek = ByteArray(32).also(random::nextBytes)
        val (iv, ct) = gcmSeal(cek, plaintext, meetingId)
        val wraps = recipients.map { (id, publicRaw) ->
            val eph = KeyPairGenerator.getInstance("EC").apply { initialize(ECGenParameterSpec("secp256r1")) }.generateKeyPair()
            val shared = KeyAgreement.getInstance("ECDH").run { init(eph.private as ECPrivateKey); doPhase(publicFromRaw(publicRaw), true); generateSecret() }
            val kek = hkdf(shared, meetingId, NOTES_INFO + id)
            val (wiv, wrapped) = gcmSeal(kek, cek, "$meetingId|$id")
            shared.fill(0); kek.fill(0)
            MeetingWrap(id, b64(rawPublic(eph.public as ECPublicKey)), b64(wiv), b64(wrapped))
        }
        cek.fill(0)
        return Sealed(b64(iv), b64(ct), wraps)
    }
    data class Sealed(val iv: String, val ciphertext: String, val wraps: List<MeetingWrap>)
}

/** A notes key held in this process (tests, and the fixture's other attendees). */
class SoftwareNotesKey(override val keyId: String = MeetingNotesCrypto.newKeyId(),
    private val pair: java.security.KeyPair = KeyPairGenerator.getInstance("EC").apply { initialize(ECGenParameterSpec("secp256r1")) }.generateKeyPair()) : NotesKey {
    override val publicKey: String = MeetingNotesCrypto.b64(MeetingNotesCrypto.rawPublic(pair.public as ECPublicKey))
    override fun agree(peer: ECPublicKey): ByteArray = KeyAgreement.getInstance("ECDH").run { init(pair.private); doPhase(peer, true); generateSecret() }
}
