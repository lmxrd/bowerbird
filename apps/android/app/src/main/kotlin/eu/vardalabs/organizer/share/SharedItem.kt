package eu.vardalabs.organizer.share

import android.content.Intent
import android.net.Uri
import androidx.core.content.IntentCompat

/** One thing shared with the app. */
sealed interface SharedItem {
    val label: String

    data class File(
        val uri: Uri,
        override val label: String,
    ) : SharedItem

    data class Link(
        val url: String,
        val title: String?,
    ) : SharedItem {
        override val label get() = title ?: url
    }

    data class Text(
        val text: String,
        val title: String?,
    ) : SharedItem {
        override val label get() = title ?: text.lineSequence().first().take(80)
    }

    companion object {
        private val URL = Regex("""https?://[^\s<>"]+""", RegexOption.IGNORE_CASE)

        /** The first web link in a text, as apps share them ("Look at this https://…"). */
        fun linkIn(text: String): String? = URL.find(text)?.value?.trimEnd('.', ',', ')', ';', '!', '?')

        /** Text shared: a link when it holds one, else the text itself. */
        fun ofText(
            text: String,
            subject: String?,
        ): SharedItem {
            val link = linkIn(text)
            return if (link != null) Link(link, subject?.ifBlank { null }) else Text(text, subject?.ifBlank { null })
        }

        /** What an `ACTION_SEND` or `ACTION_SEND_MULTIPLE` carries. */
        fun from(
            intent: Intent,
            nameOf: (Uri) -> String? = { null },
        ): List<SharedItem> {
            val streams =
                when (intent.action) {
                    Intent.ACTION_SEND -> {
                        listOfNotNull(IntentCompat.getParcelableExtra(intent, Intent.EXTRA_STREAM, Uri::class.java))
                    }

                    Intent.ACTION_SEND_MULTIPLE -> {
                        IntentCompat
                            .getParcelableArrayListExtra(intent, Intent.EXTRA_STREAM, Uri::class.java)
                            .orEmpty()
                    }

                    else -> {
                        emptyList()
                    }
                }
            if (streams.isNotEmpty()) return streams.map { File(it, nameOf(it) ?: it.lastPathSegment ?: "File") }
            val text = intent.getCharSequenceExtra(Intent.EXTRA_TEXT)?.toString()?.takeIf { it.isNotBlank() }
            return if (text != null) listOf(ofText(text, intent.getStringExtra(Intent.EXTRA_SUBJECT))) else emptyList()
        }
    }
}
