package eu.vardalabs.organizer.core.e2e

import java.io.InputStream
import java.io.OutputStream
import java.nio.ByteBuffer

/**
 * A file's contents (docs/E2E_SPEC.md §5): a header (version, the folder
 * key's generation, a random file nonce), then the contents in 64 KiB
 * segments, each sealed under a key of this file alone with its number and
 * a last-segment flag in its nonce, so that segments cannot be reordered,
 * dropped, cut or added. Streamed a segment at a time, never whole in
 * memory.
 */
object Contents {
    /** The plaintext bytes of a segment; the last one has 0 to this many. */
    const val SEGMENT = 65_536
    const val TAG = 16
    const val SEALED_SEGMENT = SEGMENT + TAG

    /** Version (1), generation (4), file nonce (32). */
    const val HEADER_LEN = 1 + 4 + 32

    /** The key of one file: HKDF of the folder key, salted with the file nonce. */
    fun fileKey(
        folderKey: ByteArray,
        fileNonce: ByteArray,
    ): ByteArray = E2e.hkdf(folderKey, fileNonce, E2e.FILE_INFO)

    /** Seven zero bytes, the segment's number and 1 for the last one, else 0. */
    fun segmentNonce(
        index: Int,
        last: Boolean,
    ): ByteArray = ByteArray(7) + E2e.u32(index) + (if (last) 1 else 0).toByte()

    /** How many segments a plaintext of [length] bytes has: at least one. */
    fun segments(length: Long): Long = maxOf(1, (length + SEGMENT - 1) / SEGMENT)

    /** The size of the encrypted file of a plaintext of [length] bytes. */
    fun encryptedSize(length: Long): Long = HEADER_LEN + length + TAG * segments(length)

    /** The size of the plaintext in an encrypted file of [length] bytes, or null when no file has that size. */
    fun plainSize(length: Long): Long? {
        if (length < HEADER_LEN + TAG) return null
        val body = length - HEADER_LEN
        val full = body / SEALED_SEGMENT
        val rest = body % SEALED_SEGMENT
        return when {
            rest == 0L -> full * SEGMENT
            rest < TAG -> null
            else -> full * SEGMENT + rest - TAG
        }
    }

    /** The generation a file was sealed with, from its header. */
    fun generation(header: ByteArray): Int {
        if (header.size < HEADER_LEN) throw E2eException("Malformed: shorter than the header")
        if (header[0] != E2e.VERSION) throw E2eException("Format version ${header[0]} is not supported")
        return ByteBuffer.wrap(header, 1, 4).int
    }

    /** Encrypt from [input] to [output], a segment at a time; answers the bytes written. */
    fun encrypt(
        folderKey: ByteArray,
        generation: Int,
        input: InputStream,
        output: OutputStream,
        fileNonce: ByteArray = E2e.random(32),
    ): Long {
        output.write(byteArrayOf(E2e.VERSION) + E2e.u32(generation) + fileNonce)
        val key = fileKey(folderKey, fileNonce)
        var written = HEADER_LEN.toLong()
        var current = ByteArray(SEGMENT)
        var filled = input.readFull(current)
        var next = ByteArray(SEGMENT)
        var index = 0
        while (true) {
            // A full segment is the last one only when nothing follows it.
            val nextFilled = if (filled == SEGMENT) input.readFull(next) else 0
            val last = nextFilled == 0
            val sealed = E2e.seal(key, segmentNonce(index, last), current.copyOf(filled), ByteArray(0))
            output.write(sealed)
            written += sealed.size
            if (last) return written
            if (index == Int.MAX_VALUE) throw E2eException("Too many segments")
            index++
            current = next.also { next = current }
            filled = nextFilled
        }
    }

    /**
     * Decrypt from [input] to [output], a segment at a time, with the key of
     * the file's generation ([keys]); answers the bytes written. [output]
     * may have received segments before a failure: write to a temporary
     * file, renamed only once this returns.
     */
    fun decrypt(
        keys: (Int) -> ByteArray?,
        input: InputStream,
        output: OutputStream,
    ): Long {
        val header = ByteArray(HEADER_LEN)
        if (input.readFull(header) < HEADER_LEN) throw E2eException("Malformed: shorter than the header")
        val generation = generation(header)
        val folderKey = keys(generation) ?: throw NoKeyException(generation)
        val key = fileKey(folderKey, header.copyOfRange(5, HEADER_LEN))
        var written = 0L
        var current = ByteArray(SEALED_SEGMENT)
        var filled = input.readFull(current)
        var next = ByteArray(SEALED_SEGMENT)
        var index = 0
        while (true) {
            if (filled < TAG) throw E2eException("Malformed: a segment shorter than its tag")
            val nextFilled = if (filled == SEALED_SEGMENT) input.readFull(next) else 0
            val last = nextFilled == 0
            val plain = E2e.open(key, segmentNonce(index, last), current, ByteArray(0), 0, filled)
            output.write(plain)
            written += plain.size
            if (last) return written
            if (index == Int.MAX_VALUE) throw E2eException("Malformed: too many segments")
            index++
            current = next.also { next = current }
            filled = nextFilled
        }
    }

    /** Fill [buffer] as far as the stream goes; answers how much was read. */
    private fun InputStream.readFull(buffer: ByteArray): Int {
        var filled = 0
        while (filled < buffer.size) {
            val read = read(buffer, filled, buffer.size - filled)
            if (read == -1) break
            filled += read
        }
        return filled
    }
}
