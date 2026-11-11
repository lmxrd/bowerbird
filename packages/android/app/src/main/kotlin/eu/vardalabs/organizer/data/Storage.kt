package eu.vardalabs.organizer.data

import eu.vardalabs.organizer.api.models.Drive
import eu.vardalabs.organizer.api.models.Unavailable

/*
 * Where a drive is stored, as its members see it (docs/M7C_DESIGN.md): the
 * server's disk, a storage backend, or someone's own storage. The app has
 * no screens to manage backends; it follows each drive's state: one whose
 * storage is unavailable offers nothing, one being moved is read-only, and
 * one on someone's own storage warns its members.
 */

/** A drive's storage now. */
sealed interface DriveStorage {
    /** Usable as usual. */
    data object Ready : DriveStorage

    /** Cannot be used now: its backend fails ([reason]), or its owner's private credentials are [locked]. */
    data class Unavailable(
        val reason: String,
        val locked: Boolean,
    ) : DriveStorage

    /** Being moved to other storage ([to], its name): read-only until done. */
    data class Moving(
        val to: String,
    ) : DriveStorage
}

/** The drive's storage now, from `available`, `unavailable`, `unavailable_reason` and `moving_to`. */
val Drive.storage: DriveStorage
    get() {
        if (!available) {
            val reason = unavailableReason ?: "Its storage cannot be reached"
            return DriveStorage.Unavailable(reason, locked = unavailable == Unavailable.LOCKED)
        }
        return movingTo?.let { DriveStorage.Moving(it.name) } ?: DriveStorage.Ready
    }

/** What the drives list says of its storage when it is not ready: the server's reason, or where it moves. */
val Drive.storageLine: String?
    get() =
        when (val storage = storage) {
            DriveStorage.Ready -> null
            is DriveStorage.Unavailable -> storage.reason
            is DriveStorage.Moving -> "Moving to ${storage.to}…"
        }

/**
 * Whether it can be opened: its folder known, its storage usable, and (one
 * from another server, `Remotes.kt`) accepted.
 */
val Drive.opens: Boolean get() = rootId != null && available && remote?.pending != true

/**
 * Whether a picker may put something new in it: it opens, and it is neither
 * being moved, nor a mirror (`Mirrors.kt`), nor on another server (`Remotes.kt`).
 */
val Drive.takesNew: Boolean get() = opens && movingTo == null && !isMirror && !isRemote

/**
 * The warning its members see on its folders when it is on someone's own
 * storage: it goes if they remove it, and the server does not back it up.
 * None for a mirror, always on its user's own storage, nor for a drive of
 * another server (its storage is that server's).
 */
val Drive.ownerWarning: String?
    get() =
        backend.owner?.takeUnless { isMirror || isRemote }?.let {
            "Stored on $it's own storage. If they remove it, this drive goes. Not backed up by the server."
        }

/** What a folder of a drive being moved says. */
fun movingNote(to: String): String = "Moving to $to… Read-only until that is done."
