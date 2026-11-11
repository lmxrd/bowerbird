package eu.vardalabs.organizer.core.e2e

import com.google.crypto.tink.subtle.X25519
import java.io.IOException
import java.nio.ByteBuffer
import java.security.GeneralSecurityException
import java.security.SecureRandom
import java.text.Normalizer
import java.util.Base64
import java.util.UUID
import javax.crypto.AEADBadTagException
import javax.crypto.Cipher
import javax.crypto.Mac
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

/**
 * Something encrypted that cannot be read or made: a wrong key or
 * passphrase, bytes that were changed, or a format this does not know. An
 * [IOException], so that the screens say it as they say a failed request.
 */
open class E2eException(
    message: String,
) : IOException(message)

/** The authentication failed: a wrong key, passphrase or recovery key, or changed bytes. */
class DecryptException : E2eException("Cannot decrypt: wrong key, or the data was changed")

/** No key of this generation is known (yet). */
class NoKeyException(
    val generation: Int,
) : E2eException("No key for generation $generation")

/**
 * The format of encrypted folders, exactly as docs/E2E_SPEC.md says and
 * `packages/e2e-crypto` (the reference) does, with the JDK's primitives
 * (AES-256-GCM, HMAC-SHA256) and Tink's X25519: the JDK's XDH needs API 33
 * on Android, Tink's runs on every version (and is on the classpath through
 * UnifiedPush's connector already).
 *
 * Pure JVM (no Android types), so that the test vectors check it.
 */
object E2e {
    /** The format's version, the first byte of everything it makes. */
    const val VERSION: Byte = 1

    /** The PBKDF2 iterations a reader accepts at least: a low count would make a guess cheap. */
    const val MIN_ITERATIONS = 100_000

    /** The most bytes (UTF-8, NFC) a name may have. */
    const val MAX_NAME_BYTES = 158

    /** A wrapped private key: version, nonce, the key and its tag. */
    const val WRAPPED_PRIVATE_LEN = 1 + 12 + 32 + 16

    /** A wrapped folder key: version, ephemeral public key, nonce, the key and its tag. */
    const val WRAPPED_FOLDER_KEY_LEN = 1 + 32 + 12 + 32 + 16

    private val PRIVATE_KEY_AD = "organizer-e2e/1/private-key".toByteArray()
    private val RECOVERY_INFO = "organizer-e2e/1/recovery".toByteArray()
    private val WRAP_INFO = "organizer-e2e/1/folder-key".toByteArray()
    private val NAME_INFO = "organizer-e2e/1/name".toByteArray()
    internal val FILE_INFO = "organizer-e2e/1/file".toByteArray()

    private const val TAG_BITS = 128
    private const val BASE32 = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567"

    private val secureRandom = SecureRandom()

    fun random(length: Int): ByteArray = ByteArray(length).also(secureRandom::nextBytes)

    // -- Primitives ----------------------------------------------------------

    /** HKDF-SHA256 (RFC 5869), 32 bytes: one block of the expansion. */
    fun hkdf(
        ikm: ByteArray,
        salt: ByteArray,
        info: ByteArray,
    ): ByteArray {
        val prk = hmac(salt, ikm)
        return hmac(prk, info + 1.toByte())
    }

    /** HMAC-SHA256. */
    private fun hmac(
        key: ByteArray,
        data: ByteArray,
    ): ByteArray {
        val mac = Mac.getInstance("HmacSHA256")
        mac.init(hmacKey(key))
        return mac.doFinal(data)
    }

    /** HMAC pads a key with zeros: an empty one, which `SecretKeySpec` refuses, is one zero byte. */
    private fun hmacKey(key: ByteArray) = SecretKeySpec(if (key.isEmpty()) ByteArray(1) else key, "HmacSHA256")

    /** AES-256-GCM: the ciphertext and its 16-byte tag. */
    fun seal(
        key: ByteArray,
        nonce: ByteArray,
        plain: ByteArray,
        ad: ByteArray,
    ): ByteArray {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, SecretKeySpec(key, "AES"), GCMParameterSpec(TAG_BITS, nonce))
        if (ad.isNotEmpty()) cipher.updateAAD(ad)
        return cipher.doFinal(plain)
    }

    /** AES-256-GCM open; a [DecryptException] when the tag does not match. */
    fun open(
        key: ByteArray,
        nonce: ByteArray,
        sealed: ByteArray,
        ad: ByteArray,
        offset: Int = 0,
        length: Int = sealed.size - offset,
    ): ByteArray {
        if (length < 16) throw E2eException("Malformed: shorter than a tag")
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, SecretKeySpec(key, "AES"), GCMParameterSpec(TAG_BITS, nonce))
        if (ad.isNotEmpty()) cipher.updateAAD(ad)
        try {
            return cipher.doFinal(sealed, offset, length)
        } catch (_: AEADBadTagException) {
            throw DecryptException()
        }
    }

    /** X25519's public key of a private key (clamped, as RFC 7748 does). */
    fun publicKey(private: ByteArray): ByteArray {
        require(private.size == 32) { "A private key is 32 bytes" }
        return X25519.publicFromPrivate(private)
    }

    /** X25519 (RFC 7748); a public key of small order (an all-zero result) is refused. */
    fun x25519(
        private: ByteArray,
        public: ByteArray,
    ): ByteArray {
        if (public.size != 32) throw E2eException("Malformed: a public key is 32 bytes")
        val shared =
            try {
                X25519.computeSharedSecret(private, public)
            } catch (e: GeneralSecurityException) {
                throw E2eException("Malformed: ${e.message ?: "not a public key"}")
            }
        if (shared.all { it == 0.toByte() }) throw E2eException("Malformed: a public key of small order")
        return shared
    }

    /** The 16 bytes of a UUID, as the reference's `Uuid::as_bytes`. */
    fun uuidBytes(id: UUID): ByteArray =
        ByteBuffer
            .allocate(16)
            .putLong(id.mostSignificantBits)
            .putLong(id.leastSignificantBits)
            .array()

    fun b64(bytes: ByteArray): String = Base64.getUrlEncoder().withoutPadding().encodeToString(bytes)

    fun unb64(text: String): ByteArray {
        if (text.contains('=')) throw E2eException("Malformed: not unpadded base64url")
        return try {
            Base64.getUrlDecoder().decode(text)
        } catch (_: IllegalArgumentException) {
            throw E2eException("Malformed: not base64url")
        }
    }

    internal fun u32(value: Int): ByteArray = ByteBuffer.allocate(4).putInt(value).array()

    private fun checkVersion(bytes: ByteArray) {
        if (bytes.isEmpty()) throw E2eException("Malformed: empty")
        if (bytes[0] != VERSION) throw E2eException("Format version ${bytes[0].toInt() and 0xff} is not supported")
    }

    private fun nfc(text: String) = Normalizer.normalize(text, Normalizer.Form.NFC)

    // -- The identity keys and the passphrase (§2) ---------------------------

    /**
     * PBKDF2-HMAC-SHA256 of the passphrase's UTF-8 bytes in NFC, 32 bytes.
     * Our own few lines over the JDK's HMAC rather than `SecretKeyFactory`:
     * that one takes a `char[]` and leaves turning it into bytes to the
     * provider (the JDK's, BouncyCastle on Android), and these bytes must
     * be the same on every client.
     */
    fun passphraseKey(
        passphrase: String,
        salt: ByteArray,
        iterations: Int,
    ): ByteArray {
        val password = nfc(passphrase).toByteArray(Charsets.UTF_8)
        val mac = Mac.getInstance("HmacSHA256")
        mac.init(hmacKey(password))
        // One block: 32 bytes out is SHA-256's length.
        mac.update(salt)
        mac.update(byteArrayOf(0, 0, 0, 1))
        val u = mac.doFinal()
        val out = u.copyOf()
        repeat(iterations - 1) {
            mac.update(u)
            mac.doFinal(u, 0)
            for (i in out.indices) out[i] = (out[i].toInt() xor u[i].toInt()).toByte()
        }
        return out
    }

    /** The private key sealed under a wrapping key: version, nonce, ciphertext and tag. */
    fun wrapPrivate(
        wrappingKey: ByteArray,
        private: ByteArray,
        nonce: ByteArray = random(12),
    ): ByteArray = byteArrayOf(VERSION) + nonce + seal(wrappingKey, nonce, private, PRIVATE_KEY_AD)

    /** The private key out of [wrapPrivate]'s bytes. */
    fun unwrapPrivate(
        wrappingKey: ByteArray,
        wrapped: ByteArray,
    ): ByteArray {
        checkVersion(wrapped)
        if (wrapped.size != WRAPPED_PRIVATE_LEN) throw E2eException("Malformed: a wrapped private key is 61 bytes")
        return open(wrappingKey, wrapped.copyOfRange(1, 13), wrapped, PRIVATE_KEY_AD, 13, wrapped.size - 13)
    }

    /** The private key out of its passphrase wrapping; too few iterations are refused. */
    fun unwrapWithPassphrase(
        wrapped: ByteArray,
        passphrase: String,
        salt: ByteArray,
        iterations: Long,
    ): ByteArray {
        if (iterations < MIN_ITERATIONS || iterations > Int.MAX_VALUE) {
            throw E2eException("Malformed: too few PBKDF2 iterations")
        }
        return unwrapPrivate(passphraseKey(passphrase, salt, iterations.toInt()), wrapped)
    }

    // -- The recovery key (§3) -----------------------------------------------

    fun recoveryWrappingKey(recovery: ByteArray): ByteArray = hkdf(recovery, ByteArray(0), RECOVERY_INFO)

    fun unwrapWithRecovery(
        wrapped: ByteArray,
        recovery: ByteArray,
    ): ByteArray = unwrapPrivate(recoveryWrappingKey(recovery), wrapped)

    /** The recovery key as shown: RFC 4648 base32, no padding, 13 groups of 4 joined by `-`. */
    fun encodeRecovery(recovery: ByteArray): String {
        require(recovery.size == 32) { "A recovery key is 32 bytes" }
        val text = StringBuilder()
        var buffer = 0
        var bits = 0
        for (byte in recovery) {
            buffer = (buffer shl 8) or (byte.toInt() and 0xff)
            bits += 8
            while (bits >= 5) {
                text.append(BASE32[(buffer shr (bits - 5)) and 31])
                bits -= 5
            }
        }
        if (bits > 0) text.append(BASE32[(buffer shl (5 - bits)) and 31])
        return text.chunked(4).joinToString("-")
    }

    /**
     * The recovery key as typed: dashes and white space ignored, lowercase
     * read as uppercase. Anything else is refused, unused bits that are not
     * zero (a typo) included.
     */
    fun decodeRecovery(text: String): ByteArray {
        val clean =
            text
                .filter { it != '-' && !it.isWhitespace() }
                .map { if (it in 'a'..'z') it.uppercaseChar() else it }
                .joinToString("")
        if (clean.length != 52 || clean.any { it !in BASE32 }) {
            throw E2eException("Malformed: a recovery key is 52 base32 characters")
        }
        val out = ByteArray(32)
        var buffer = 0
        var bits = 0
        var at = 0
        for (c in clean) {
            buffer = (buffer shl 5) or BASE32.indexOf(c)
            bits += 5
            if (bits >= 8) {
                if (at == 32) break
                out[at++] = (buffer shr (bits - 8)).toByte()
                bits -= 8
            }
        }
        // 260 bits for 256: the last four must be zero.
        if (encodeRecovery(out).replace("-", "") != clean) throw E2eException("Malformed: not a recovery key")
        return out
    }

    // -- Folder keys (§4) ----------------------------------------------------

    /** The associated data of a wrapping: the folder's 16 bytes and the generation. */
    private fun wrapAd(
        folder: UUID,
        generation: Int,
    ) = uuidBytes(folder) + u32(generation)

    private fun wrappingKey(
        shared: ByteArray,
        ephemeralPublic: ByteArray,
        recipient: ByteArray,
    ) = hkdf(shared, ephemeralPublic + recipient, WRAP_INFO)

    /** The folder key wrapped for the holder of [recipient] (a public key). */
    fun wrapFolderKey(
        folderKey: ByteArray,
        folder: UUID,
        generation: Int,
        recipient: ByteArray,
        ephemeral: ByteArray = random(32),
        nonce: ByteArray = random(12),
    ): ByteArray {
        val ephemeralPublic = publicKey(ephemeral)
        val shared = x25519(ephemeral, recipient)
        val key = wrappingKey(shared, ephemeralPublic, recipient)
        return byteArrayOf(VERSION) + ephemeralPublic + nonce + seal(key, nonce, folderKey, wrapAd(folder, generation))
    }

    /** The folder key out of a wrapping for the holder of [private]. */
    fun unwrapFolderKey(
        wrapped: ByteArray,
        folder: UUID,
        generation: Int,
        private: ByteArray,
    ): ByteArray {
        checkVersion(wrapped)
        if (wrapped.size != WRAPPED_FOLDER_KEY_LEN) throw E2eException("Malformed: a wrapped folder key is 93 bytes")
        val ephemeralPublic = wrapped.copyOfRange(1, 33)
        val nonce = wrapped.copyOfRange(33, 45)
        val shared = x25519(private, ephemeralPublic)
        val key = wrappingKey(shared, ephemeralPublic, publicKey(private))
        return open(key, nonce, wrapped, wrapAd(folder, generation), 45, wrapped.size - 45)
    }

    // -- Names (§6) ----------------------------------------------------------

    fun nameKey(folderKey: ByteArray): ByteArray = hkdf(folderKey, ByteArray(0), NAME_INFO)

    /** A name in NFC, checked: not empty, at most [MAX_NAME_BYTES] bytes. */
    fun checkName(name: String): String {
        val normal = nfc(name)
        if (normal.isEmpty()) throw E2eException("A name cannot be empty")
        if (normal.toByteArray(Charsets.UTF_8).size > MAX_NAME_BYTES) {
            throw E2eException("The name is too long for an encrypted folder")
        }
        return normal
    }

    /** A name sealed for the folder [parent]: base64url of version, generation, nonce, ciphertext and tag. */
    fun encryptName(
        folderKey: ByteArray,
        generation: Int,
        parent: UUID,
        name: String,
        nonce: ByteArray = random(12),
    ): String {
        val plain = checkName(name).toByteArray(Charsets.UTF_8)
        return b64(
            byteArrayOf(VERSION) + u32(generation) + nonce +
                seal(nameKey(folderKey), nonce, plain, uuidBytes(parent)),
        )
    }

    /** The generation of the folder key a name was sealed with. */
    fun nameGeneration(encrypted: String): Int {
        val bytes = unb64(encrypted)
        checkVersion(bytes)
        if (bytes.size < 1 + 4 + 12 + 16) throw E2eException("Malformed: too short for a name")
        return ByteBuffer.wrap(bytes, 1, 4).int
    }

    /** The name out of [encryptName], with the key of its generation. */
    fun decryptName(
        folderKey: ByteArray,
        parent: UUID,
        encrypted: String,
    ): String {
        val bytes = unb64(encrypted)
        checkVersion(bytes)
        if (bytes.size < 1 + 4 + 12 + 16) throw E2eException("Malformed: too short for a name")
        val plain = open(nameKey(folderKey), bytes.copyOfRange(5, 17), bytes, uuidBytes(parent), 17, bytes.size - 17)
        val decoder = Charsets.UTF_8.newDecoder()
        return try {
            decoder.decode(ByteBuffer.wrap(plain)).toString()
        } catch (_: java.nio.charset.CharacterCodingException) {
            throw E2eException("Malformed: not UTF-8")
        }
    }
}
