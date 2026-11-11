package eu.vardalabs.organizer.data

import eu.vardalabs.organizer.api.models.Entry
import eu.vardalabs.organizer.api.models.Kind
import eu.vardalabs.organizer.api.models.MoveFile
import eu.vardalabs.organizer.api.models.NewFolder
import eu.vardalabs.organizer.api.models.Node
import eu.vardalabs.organizer.api.models.Role
import eu.vardalabs.organizer.core.api.Api
import eu.vardalabs.organizer.core.api.ApiException
import eu.vardalabs.organizer.core.api.apiCall
import eu.vardalabs.organizer.core.e2e.Contents
import eu.vardalabs.organizer.core.e2e.E2e
import eu.vardalabs.organizer.core.e2e.E2eException
import eu.vardalabs.organizer.core.e2e.Keyring
import eu.vardalabs.organizer.core.e2e.NoKeyException
import eu.vardalabs.organizer.core.upload.UploadClient
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.runInterruptible
import java.io.File
import java.io.IOException
import java.text.Normalizer
import java.util.Locale
import java.util.UUID

/** A folder as the screens show it: its path from its drive's root (`/` for the root). */
data class Folder(
    val id: UUID,
    val name: String,
    val path: String,
    val parentId: UUID?,
    /** The end-to-end-encrypted folder it is (its own id) or is in; null when plain. */
    val e2eRoot: UUID? = null,
    /** The name of the user's drive it is in; null for one shared with them from another's. */
    val driveName: String? = null,
    /** What the user may do in it (`my_role`): see `Roles.kt`. */
    val role: Role? = null,
    /** Its drive is being moved to this storage (`Storage.kt`): read-only until done. */
    val movingTo: String? = null,
    /** Its drive is on someone's own storage: the warning it shows. */
    val storageWarning: String? = null,
    /** Its drive is a mirror (`Mirrors.kt`): a read-only copy, always. */
    val mirror: Boolean = false,
    /** Shared from another server (`Remotes.kt`): that server; null for this server's. */
    val remote: String? = null,
) {
    /** Its drive is being moved, or is a mirror: nothing in it offers an edit. */
    val readOnly: Boolean get() = movingTo != null || mirror

    /** Where it is, starting at its drive: `My drive`, `Team/Plans`. */
    val location: String
        get() =
            when {
                driveName == null -> path.ifEmpty { name }
                path == "/" -> driveName
                else -> "$driveName$path"
            }
}

/** What a name that does not decrypt shows. */
const val ENCRYPTED_ITEM = "Encrypted item"

/** Inside an encrypted folder (not the folder itself): its name is sealed. */
val Node.sealed: Boolean get() = e2eRoot != null && e2eRoot != id

/**
 * The files and folders of the signed-in user. Under an encrypted folder,
 * what it answers has the names decrypted (and the sizes of the plaintext),
 * and what it is given is encrypted before it goes to the server.
 */
interface FilesRepository {
    /** A folder, or the root for null. */
    suspend fun folder(id: UUID?): Folder

    /** What is in a folder: folders first, then files, by name (all pages). */
    suspend fun children(id: UUID): List<Node>

    suspend fun createFolder(
        parent: Folder,
        name: String,
    ): Node

    suspend fun rename(
        node: Node,
        name: String,
    ): Node

    suspend fun move(
        node: Node,
        to: UUID,
    ): Node

    /** To the trash. */
    suspend fun delete(node: Node)

    /** The file's content in the cache, downloaded (and decrypted) unless this version is there already. */
    suspend fun download(node: Node): File
}

/** Thrown when there is no session (signed out meanwhile). */
class SignedOutException : ApiException(401, "Signed out")

fun Entry.toFolder() = Folder(id, name.ifEmpty { "Files" }, path, parentId, e2eRoot, role = myRole, remote = remote)

/** A name as clashes are told: the server refuses two that differ only in case. */
fun nameKey(name: String): String = Normalizer.normalize(name.trim(), Normalizer.Form.NFC).lowercase(Locale.ROOT)

class ApiFilesRepository(
    private val api: suspend () -> Api?,
    private val cacheDir: File,
    /** Names the drive a folder is in, and tells a drive's root. */
    private val drives: ApiDrivesRepository,
    /** The keys of an encrypted folder; read again from the server when `fresh`. */
    private val keyring: suspend (root: UUID, fresh: Boolean) -> Keyring,
) : FilesRepository {
    private suspend fun api() = api.invoke() ?: throw SignedOutException()

    override suspend fun folder(id: UUID?): Folder {
        val api = api()
        val entry = apiCall { if (id == null) api.files.getRoot() else api.files.getFile(id) }
        // Titles and paths start at the drive; a drive's root is called by the
        // drive's name (a mirror's says it is one).
        val drive = drives.drive(entry.driveId)
        val driveName = drive?.title
        val folder =
            entry
                .toFolder()
                .let {
                    if (entry.parentId == null && driveName != null) {
                        it.copy(name = driveName, driveName = driveName)
                    } else {
                        it.copy(driveName = driveName)
                    }
                }.copy(
                    movingTo = drive?.movingTo?.name,
                    storageWarning = drive?.ownerWarning,
                    mirror = drive?.isMirror == true,
                )
        val root = entry.e2eRoot
        val parent = entry.parentId
        if (root == null || root == entry.id || parent == null) return folder
        // Inside an encrypted folder its name is sealed, and so is its path's
        // below the encrypted folder (whose own name is plain).
        val remote = entry.remote != null
        val name =
            try {
                keyring(sealedWith(entry.driveId, root, remote), false)
                    .decryptName(sealedWith(entry.driveId, parent, remote), entry.name)
            } catch (_: E2eException) {
                ENCRYPTED_ITEM
            }
        val top = apiCall { api.files.getFile(root) }.path
        return folder.copy(name = name, path = if (parent == root) "$top/$name" else "$top/…/$name")
    }

    override suspend fun children(id: UUID): List<Node> {
        val api = api()
        val all = mutableListOf<Node>()
        var after: String? = null
        do {
            val page = apiCall { api.files.listChildren(id, after = after, limit = 1000) }
            all += page.items
            after = page.next
        } while (after != null)
        // The server never lists a drive's own folder; should one show up, it is still not the user's.
        if (all.any { it.name == DRIVE_DATA } && drives.isRoot(all.first().driveId, id)) {
            all.removeAll { it.name == DRIVE_DATA }
        }
        val sealed = all.firstOrNull { it.sealed } ?: return all
        val root = sealed.e2eRoot ?: return all
        val remote = sealed.remote != null
        return decrypted(sealedWith(sealed.driveId, root, remote), sealedWith(sealed.driveId, id, remote), all)
    }

    private val sources =
        MirrorSources(
            isMirror = { drives.drive(it)?.isMirror == true },
            sourceOf = { id ->
                val api = api()
                apiCall { api.files.getFile(id) }.sourceId
            },
        )

    private suspend fun sealedWith(
        driveId: String,
        id: UUID,
        remote: Boolean,
    ): UUID = sources.sealedWith(driveId, id, remote)

    /**
     * The id names in [folder] are sealed with: its own, but shared from
     * another server (`remote`) its id there (`source_id`), asked again
     * while not known (docs/E2E_SPEC.md §12).
     */
    suspend fun sealingId(folder: Folder): UUID = sealingId(folder.id, folder.remote != null)

    private suspend fun sealingId(
        id: UUID,
        remote: Boolean,
    ): UUID = if (remote) sources.sealingFor(id) else id

    /** The names opened (one that does not open shows as [ENCRYPTED_ITEM]), sorted again by them. */
    private suspend fun decrypted(
        root: UUID,
        parent: UUID,
        nodes: List<Node>,
    ): List<Node> {
        var ring = keyring(root, false)
        // A generation made since the keys were read: read them again.
        val unknown =
            nodes.any { node ->
                val generation = runCatching { E2e.nameGeneration(node.name) }.getOrNull()
                generation != null && ring.key(generation) == null
            }
        if (unknown) ring = keyring(root, true)
        return nodes
            .map { node ->
                val name =
                    try {
                        ring.decryptName(parent, node.name)
                    } catch (_: E2eException) {
                        ENCRYPTED_ITEM
                    }
                val size =
                    if (node.kind ==
                        Kind.FILE
                    ) {
                        Contents.plainSize(node.propertySize) ?: node.propertySize
                    } else {
                        node.propertySize
                    }
                node.copy(name = name, propertySize = size)
            }.sortedWith(compareBy<Node> { it.kind != Kind.FOLDER }.thenBy(String.CASE_INSENSITIVE_ORDER) { it.name })
    }

    /**
     * The name to send for [name] in the folder [parent]: under an encrypted
     * folder sealed with its newest key (for the parent's sealing id: from
     * another server, its id there), once no sibling (but [except]) has it,
     * which only a device can tell there.
     */
    private suspend fun nameFor(
        root: UUID?,
        parent: UUID,
        name: String,
        except: UUID? = null,
        remote: Boolean = false,
    ): String {
        val trimmed = name.trim()
        if (root == null) return trimmed
        val sealing = sealingId(parent, remote)
        val ring = keyring(root, true)
        val key = nameKey(trimmed)
        if (children(parent).any { it.id != except && nameKey(it.name) == key }) {
            throw ApiException(409, "$trimmed already exists")
        }
        return ring.encryptName(sealing, trimmed)
    }

    override suspend fun createFolder(
        parent: Folder,
        name: String,
    ): Node {
        val api = api()
        val sealed = nameFor(parent.e2eRoot, parent.id, name, remote = parent.remote != null)
        return apiCall { api.files.createFolder(parent.id, NewFolder(sealed)) }
    }

    override suspend fun rename(
        node: Node,
        name: String,
    ): Node {
        val api = api()
        val parent = node.parentId
        val sealed =
            if (node.sealed && parent != null) {
                nameFor(node.e2eRoot, parent, name, node.id, node.remote != null)
            } else {
                name.trim()
            }
        return apiCall { api.files.moveFile(node.id, MoveFile(ifVersion = node.version, name = sealed)) }
    }

    override suspend fun move(
        node: Node,
        to: UUID,
    ): Node {
        // A move inside an encrypted folder needs its name sealed again for the new parent: not offered here.
        if (node.sealed) throw E2eException("Move it on the web")
        val api = api()
        return apiCall { api.files.moveFile(node.id, MoveFile(ifVersion = node.version, parentId = to)) }
    }

    override suspend fun delete(node: Node) {
        val api = api()
        apiCall { api.files.deleteFile(node.id, node.version) }
    }

    override suspend fun download(node: Node): File {
        require(node.kind == Kind.FILE) { "Not a file" }
        val api = api()
        // One folder per file and version: a changed file is downloaded again.
        val dir = File(cacheDir, "open/${node.id}/${node.version}")
        val file = File(dir, safeName(node.name))
        if (file.exists()) return file
        File(cacheDir, "open/${node.id}").deleteRecursively()
        val client = UploadClient(api.http, api.server)
        val root = node.e2eRoot
        if (root == null) {
            runInterruptible(Dispatchers.IO) { client.download(node.id, file) }
            return file
        }
        val keys = sealedWith(node.driveId, root, node.remote != null)
        try {
            decryptTo(client, node.id, keyring(keys, false), file)
        } catch (_: NoKeyException) {
            // A generation made since the keys were read.
            decryptTo(client, node.id, keyring(keys, true), file)
        }
        return file
    }

    /** Download and decrypt a segment at a time, into a temporary file renamed once the last segment checked. */
    private suspend fun decryptTo(
        client: UploadClient,
        id: UUID,
        ring: Keyring,
        file: File,
    ) = runInterruptible(Dispatchers.IO) {
        file.parentFile?.mkdirs()
        val partial = File(file.path + ".part")
        try {
            client.read(id) { input -> partial.outputStream().buffered().use { ring.decrypt(input, it) } }
            if (!partial.renameTo(file)) throw IOException("Cannot save ${file.name}")
        } finally {
            partial.delete()
        }
    }

    private fun safeName(name: String) = name.replace('/', '_').ifEmpty { "file" }
}
