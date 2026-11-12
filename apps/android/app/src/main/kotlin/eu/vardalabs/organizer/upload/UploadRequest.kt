package eu.vardalabs.organizer.upload

import androidx.work.Data
import androidx.work.workDataOf

/** Where something sent goes. */
enum class UploadKind {
    /** A file into a folder. */
    FOLDER,

    /** A file into the drop box. */
    DROP_BOX,

    /** A link into the drop box. */
    LINK,
}

/** One upload, as the worker's input. */
data class UploadRequest(
    val kind: UploadKind,
    /** The file's name (or the link, for a link). */
    val name: String,
    /** A `content://` URI the app may read, or the path of the app's own copy. */
    val source: String? = null,
    val size: Long = 0,
    /** [source] is the app's own copy, deleted once uploaded. */
    val staged: Boolean = false,
    val folderId: String? = null,
    val note: String? = null,
    /** Null for the user's setting. */
    val readContents: Boolean? = null,
    /** For a link: `bookmark`, `snapshot` or `download`; null for the user's setting. */
    val linkMode: String? = null,
    val url: String? = null,
    /**
     * Into an encrypted folder: the name as sent, sealed on this device
     * ([name] is the plain one, for the screens), and [source] the staged
     * copy of the encrypted contents.
     */
    val encryptedName: String? = null,
) {
    fun toData(): Data =
        workDataOf(
            KIND to kind.name,
            NAME to name,
            SOURCE to source,
            SIZE to size,
            STAGED to staged,
            FOLDER to folderId,
            NOTE to note,
            READ_CONTENTS to readContents?.toString(),
            LINK_MODE to linkMode,
            URL to url,
            ENCRYPTED_NAME to encryptedName,
        )

    companion object {
        private const val KIND = "kind"
        private const val NAME = "name"
        private const val SOURCE = "source"
        private const val SIZE = "size"
        private const val STAGED = "staged"
        private const val FOLDER = "folder"
        private const val NOTE = "note"
        private const val READ_CONTENTS = "read_contents"
        private const val LINK_MODE = "link_mode"
        private const val URL = "url"
        private const val ENCRYPTED_NAME = "encrypted_name"

        fun from(data: Data) =
            UploadRequest(
                kind = UploadKind.valueOf(data.getString(KIND) ?: UploadKind.FOLDER.name),
                name = data.getString(NAME).orEmpty(),
                source = data.getString(SOURCE),
                size = data.getLong(SIZE, 0),
                staged = data.getBoolean(STAGED, false),
                folderId = data.getString(FOLDER),
                note = data.getString(NOTE),
                readContents = data.getString(READ_CONTENTS)?.toBooleanStrictOrNull(),
                linkMode = data.getString(LINK_MODE),
                url = data.getString(URL),
                encryptedName = data.getString(ENCRYPTED_NAME),
            )
    }
}
