package eu.vardalabs.organizer.data

import eu.vardalabs.organizer.api.models.Drive
import eu.vardalabs.organizer.api.models.DriveKind
import eu.vardalabs.organizer.api.models.IncomingShareState
import eu.vardalabs.organizer.api.models.RemoteDrive
import eu.vardalabs.organizer.api.models.SharedItem

/*
 * Shares from other servers (docs/M7E_DESIGN.md §5): their bytes go through
 * the user's own server, so a remote folder is a folder and a remote drive a
 * drive, browsed by `my_role` as any (`Roles.kt`). What the app adds: the
 * drives list shows the remote drives in a group of their own, "From other
 * servers", with whose they are, where, and how they stand; Shared with me
 * marks what came from another server, with Accept and Decline while it
 * waits; and nothing new is ever put in one by a picker (`takesNew`, the
 * folder picker's own check of `Folder.remote`). The app never shares with
 * someone on another server (on the web).
 */

/** A drive shared from another server. */
val Drive.isRemote: Boolean get() = kind == DriveKind.REMOTE

/** Waiting for the user to accept it: its files are not listed until then. */
val RemoteDrive.pending: Boolean get() = state == IncomingShareState.PENDING

/** Whose it is and where: `From alice@cloud.example` (the server named apart when the address does not say it). */
val RemoteDrive.fromLine: String
    get() = if (owner.endsWith("@$server")) "From $owner" else "From $owner on $server"

/** How it stands, when it is not simply there: waiting for Accept, or its server failing (and why); else null. */
val RemoteDrive.stateLine: String?
    get() =
        when {
            pending -> "Waiting for you to accept it"
            error != null -> "Its server cannot be reached now: $error"
            else -> null
        }

/** Shared with the user from another server and not accepted yet: Accept or Decline. */
val SharedItem.awaitsAnswer: Boolean get() = remote?.pending == true

/** Who shared it, as Shared with me says: the address on another server, else the user here. */
val SharedItem.fromLine: String get() = remote?.fromLine ?: "From ${sharedBy.email}"
