package eu.vardalabs.organizer.ui.common

import java.util.Locale
import kotlin.math.roundToInt

/** A size in bytes as people read it: `512 B`, `1.4 MB`. */
fun formatSize(bytes: Long): String {
    if (bytes < 1024) return "$bytes B"
    val units = listOf("KB", "MB", "GB", "TB")
    var value = bytes / 1024.0
    var unit = 0
    while (value >= 1024 && unit < units.lastIndex) {
        value /= 1024
        unit++
    }
    val number = if (value < 10) String.format(Locale.ROOT, "%.1f", value) else value.roundToInt().toString()
    return "$number ${units[unit]}"
}

/** A confidence from 0 to 1 as a whole percent, as the web shows it. */
fun percent(confidence: Double): String = "${(confidence.coerceIn(0.0, 1.0) * 100).roundToInt()}%"

/** The date of an ISO 8601 timestamp (`2026-11-07`), or the timestamp. */
fun formatDate(iso: String): String = iso.take(10)
