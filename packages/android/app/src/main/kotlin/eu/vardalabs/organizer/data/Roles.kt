package eu.vardalabs.organizer.data

import eu.vardalabs.organizer.api.models.Role

/*
 * What the app offers for a role (`my_role`, docs/M7B_DESIGN.md §1 and §11),
 * as the server allows it (`files/roles.rs`): upload only adds files and
 * folders, a viewer only reads, an editor and above also change and delete.
 * The server checks each request again; this only keeps the screens from
 * offering what would be refused. A missing role (none of the user's
 * requests should have one) allows nothing but reading.
 */

/** Whether files and folders may be added in a folder with this role (upload, new folder, a place to put things). */
val Role?.mayAdd: Boolean
    get() = this != null && this != Role.VIEWER

/** Whether an item with this role may be renamed, moved or deleted (to the trash). */
val Role?.mayChange: Boolean
    get() = this == Role.EDITOR || this == Role.MANAGER || this == Role.OWNER

/** Upload only: the folder lists only what the user added. */
val Role?.onlyOwnUploads: Boolean
    get() = this == Role.UPLOAD_ONLY

/** What may be done with a file or folder of a listing. */
data class ItemActions(
    val rename: Boolean,
    val move: Boolean,
    val delete: Boolean,
) {
    /** Whether its menu has anything. */
    val any: Boolean get() = rename || move || delete

    companion object {
        /**
         * For an item with [role]. [sealed]: in an encrypted folder, where a
         * move seals the name again (offered on the web only).
         */
        fun of(
            role: Role?,
            sealed: Boolean = false,
        ) = ItemActions(rename = role.mayChange, move = role.mayChange && !sealed, delete = role.mayChange)
    }
}
