package eu.vardalabs.organizer.data

import eu.vardalabs.organizer.api.models.Drive
import eu.vardalabs.organizer.api.models.DriveKind
import eu.vardalabs.organizer.api.models.MirrorStatus
import eu.vardalabs.organizer.api.models.MirrorStopped
import eu.vardalabs.organizer.core.e2e.E2eException
import java.util.UUID
import java.util.concurrent.ConcurrentHashMap

/*
 * Mirrors (docs/M7D_DESIGN.md): read-only copies of drives in the user's
 * own storage. The app has no screens of their own: the drives list shows
 * them in a group after the shared drives, labelled as mirrors with their
 * state, and they are browsed as a viewer's drive (`my_role` viewer).
 * Nothing is ever put in one (`takesNew`).
 */

/**
 * The ids an encrypted folder's keys and names are sealed with, for the
 * nodes of a drive: their own, but in a mirror, or shared from another
 * server (docs/E2E_SPEC.md §12, `remote`), the node each copies there
 * (`source_id`, kept once known): such an encrypted folder has no keys of
 * its own (they are asked by its source's id, or its own, and unwrapped with
 * the answer's `folder_id`), and its names were sealed for the source's
 * folders.
 */
class MirrorSources(
    private val isMirror: suspend (driveId: String) -> Boolean,
    private val sourceOf: suspend (id: UUID) -> UUID?,
) {
    private val known = ConcurrentHashMap<UUID, UUID>()

    /** The id [id]'s names open with: its source when it has one known, else its own. */
    suspend fun sealedWith(
        driveId: String,
        id: UUID,
        remote: Boolean = false,
    ): UUID {
        if (!remote && !isMirror(driveId)) return id
        return source(id) ?: id
    }

    /**
     * The id a new name in the folder [id] of another server's encrypted
     * folder is sealed with (a new file or folder, a rename): its source.
     * A folder made a moment ago may have none yet: asked again, and
     * refused while it still has none.
     */
    suspend fun sealingFor(id: UUID): UUID = source(id) ?: throw NotOnOwnersServerException()

    private suspend fun source(id: UUID): UUID? {
        known[id]?.let { return it }
        val source = sourceOf(id) ?: return null
        known[id] = source
        return source
    }
}

/** A folder shared from another server that is not there yet (no `source_id`): nothing is sealed in it. */
class NotOnOwnersServerException : E2eException("This folder isn't on its owner's server yet. Try again in a moment.")

/** A mirror: a read-only copy of a drive. */
val Drive.isMirror: Boolean get() = kind == DriveKind.MIRROR

/** Its name as listed: a mirror's says it is one (`Team · mirror`). */
val Drive.title: String get() = if (isMirror) "$name · mirror" else name

/** Why a mirror stopped copying, in words (the server's `Stopped::reason`). */
val MirrorStopped.reason: String
    get() =
        when (this) {
            MirrorStopped.DRIVE_DELETED -> "The drive was deleted"
            MirrorStopped.ACCESS_LOST -> "You no longer have this drive (or may only upload to it)"
            MirrorStopped.FORBIDDEN -> "The drive's managers do not allow mirroring it"
            MirrorStopped.SHARED_DRIVES_OFF -> "Mirroring shared drives is not allowed here"
        }

/**
 * Where a mirror stands, by the status's code (never its words): up to date,
 * copying, waiting for its storage's unlock, failing, or stopped and why;
 * null for another drive.
 */
val Drive.mirrorLine: String?
    get() {
        val mirror = mirrorOf?.takeIf { isMirror } ?: return null
        return when (mirror.status) {
            MirrorStatus.UP_TO_DATE -> "Up to date"
            MirrorStatus.PENDING -> "Changes wait for the next copy"
            MirrorStatus.COPYING -> "Copying…"
            MirrorStatus.WAITING_FOR_UNLOCK -> "Waiting for your storage to be unlocked"
            MirrorStatus.FAILING -> "Copying fails; it is tried again in a few minutes"
            MirrorStatus.STOPPED -> "Stopped: ${mirror.stopped?.reason ?: "it no longer copies"}"
        }
    }
