package eu.vardalabs.organizer.data

import eu.vardalabs.organizer.api.models.Drive
import eu.vardalabs.organizer.api.models.SharedItem
import eu.vardalabs.organizer.core.api.Api
import eu.vardalabs.organizer.core.api.apiCall
import java.util.UUID

/** Where each drive keeps its trash and versions on the server: never listed, never shown. */
const val DRIVE_DATA = ".organizer"

/** The user's drives and what is shared with them: where the Files tab starts. */
interface DrivesRepository {
    /** My drive first, then the shared drives the user is a member of, by name. */
    suspend fun drives(): List<Drive>

    /** What is shared with the user, the newest first. */
    suspend fun sharedWithMe(): List<SharedItem>

    /** Accept what another server shared (`Remotes.kt`): the shared item's id, or a remote drive's root. */
    suspend fun accept(id: UUID)

    /** Decline something shared with the user (from another server: it is told). */
    suspend fun decline(id: UUID)
}

class ApiDrivesRepository(
    private val api: suspend () -> Api?,
) : DrivesRepository {
    private suspend fun api() = api.invoke() ?: throw SignedOutException()

    /** The drives as last read: the folders' titles name their drive without asking each time. */
    @Volatile private var known: List<Drive> = emptyList()

    /** Drive ids asked about once and not the user's (a share's): not read again for them. */
    private val foreign = mutableSetOf<String>()

    override suspend fun drives(): List<Drive> {
        val api = api()
        return apiCall { api.drives.listDrives() }.also { known = it }
    }

    override suspend fun sharedWithMe(): List<SharedItem> {
        val api = api()
        return apiCall { api.sharing.listSharedWithMe() }
    }

    override suspend fun accept(id: UUID) {
        val api = api()
        apiCall { api.sharing.acceptShared(id) }
    }

    override suspend fun decline(id: UUID) {
        val api = api()
        apiCall { api.sharing.declineShared(id) }
    }

    /** A drive of the user, read again once when it is not known yet (joined meanwhile); null for another's. */
    suspend fun drive(id: String): Drive? {
        known.firstOrNull { it.id == id }?.let { return it }
        if (synchronized(foreign) { id in foreign }) return null
        drives()
        return known.firstOrNull { it.id == id } ?: run {
            synchronized(foreign) { foreign += id }
            null
        }
    }

    /** Whether the folder is the root of one of the user's drives. */
    suspend fun isRoot(
        driveId: String,
        id: UUID,
    ): Boolean = drive(driveId)?.rootId == id
}
