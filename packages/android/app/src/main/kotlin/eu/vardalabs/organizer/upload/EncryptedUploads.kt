package eu.vardalabs.organizer.upload

import eu.vardalabs.organizer.core.e2e.Keyring
import eu.vardalabs.organizer.core.upload.UploadClient
import eu.vardalabs.organizer.data.Folder
import eu.vardalabs.organizer.data.nameKey
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.withContext
import java.io.InputStream
import java.io.OutputStream
import java.util.UUID

/**
 * Uploads into an encrypted folder (docs/E2E_SPEC.md §5, §6): each name
 * made free here (the server cannot compare sealed names, so the siblings'
 * are decrypted), then sealed with the folder's id (shared from another
 * server, its id there: [sealingId]), and the contents
 * sealed into a staged copy, both with the newest key; the worker sends
 * that copy as any other file (plainly under 4 MB, resumably above, by its
 * encrypted size).
 */
class EncryptedUploads(
    /** The keys of an encrypted folder, read again when `fresh`. */
    private val keyring: suspend (root: UUID, fresh: Boolean) -> Keyring,
    /** The decrypted names of what is in a folder. */
    private val names: suspend (folder: UUID) -> List<String>,
    private val seal: (Prepared, (InputStream, OutputStream) -> Unit) -> Prepared,
    /** The id names in a folder are sealed with (docs/E2E_SPEC.md §6, §12). */
    private val sealingId: suspend (folder: Folder) -> UUID = { it.id },
) {
    suspend fun requests(
        folder: Folder,
        prepared: List<Prepared>,
    ): List<UploadRequest> {
        val root = requireNotNull(folder.e2eRoot) { "Not an encrypted folder" }
        // The newest generation: a member who lost access must not read what is written now.
        val sealing = sealingId(folder)
        val ring = keyring(root, true)
        val taken = names(folder.id).map(::nameKey).toMutableSet()
        return withContext(Dispatchers.IO) {
            prepared.map { item ->
                val name = free(item.name, taken)
                val sealedName = ring.encryptName(sealing, name)
                val sealed = seal(item) { input, output -> ring.encrypt(input, output) }
                UploadRequest(
                    kind = UploadKind.FOLDER,
                    name = name,
                    source = sealed.source,
                    size = sealed.size,
                    staged = true,
                    folderId = folder.id.toString(),
                    encryptedName = sealedName,
                )
            }
        }
    }

    companion object {
        /** [name], or `name (2)`, `name (3)`, ...: the first not [taken], which it then is. */
        fun free(
            name: String,
            taken: MutableSet<String>,
        ): String {
            var n = 1
            while (nameKey(UploadClient.numbered(name, n)) in taken) n++
            return UploadClient.numbered(name, n).also { taken += nameKey(it) }
        }
    }
}
