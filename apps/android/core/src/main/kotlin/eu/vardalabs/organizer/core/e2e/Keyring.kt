package eu.vardalabs.organizer.core.e2e

import java.io.InputStream
import java.io.OutputStream
import java.util.UUID

/**
 * The keys of one encrypted folder (its top, [folderId]), unwrapped: every
 * generation this user has, and the newest, which new names and contents
 * use. What is under the folder opens with the generation it was sealed
 * with.
 */
class Keyring(
    val folderId: UUID,
    /** The newest generation; 0 while the folder has no key yet. */
    val generation: Int,
    private val keys: Map<Int, ByteArray>,
) {
    fun key(generation: Int): ByteArray? = keys[generation]

    /** The newest key: new names and contents are sealed with it. */
    private fun newest(): ByteArray =
        keys[generation] ?: throw E2eException("This encrypted folder has no key on this device yet")

    /** A name sealed for the folder [parent], with the newest key. */
    fun encryptName(
        parent: UUID,
        name: String,
    ): String = E2e.encryptName(newest(), generation, parent, name)

    /** A name of the folder [parent] opened, with the key of its generation. */
    fun decryptName(
        parent: UUID,
        encrypted: String,
    ): String {
        val generation = E2e.nameGeneration(encrypted)
        val key = keys[generation] ?: throw NoKeyException(generation)
        return E2e.decryptName(key, parent, encrypted)
    }

    /** Contents sealed with the newest key, streamed; answers the bytes written. */
    fun encrypt(
        input: InputStream,
        output: OutputStream,
    ): Long = Contents.encrypt(newest(), generation, input, output)

    /** Contents opened with the key of their generation, streamed; answers the bytes written. */
    fun decrypt(
        input: InputStream,
        output: OutputStream,
    ): Long = Contents.decrypt(keys::get, input, output)
}
