package eu.vardalabs.organizer.ui.common

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.outlined.CloudUpload
import androidx.compose.material.icons.outlined.ErrorOutline
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.LinearProgressIndicator
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import eu.vardalabs.organizer.upload.UploadInfo

/** An upload in progress, waiting, or failed (with a way to dismiss it). */
@Composable
fun UploadRow(
    upload: UploadInfo,
    onDismiss: () -> Unit,
) {
    val failed = upload.state == UploadInfo.State.FAILED
    ListItem(
        headlineContent = { Text(upload.name, maxLines = 1, overflow = TextOverflow.Ellipsis) },
        supportingContent = {
            Column {
                when (upload.state) {
                    UploadInfo.State.FAILED -> {
                        Text(upload.error ?: "The upload failed", color = MaterialTheme.colorScheme.error)
                    }

                    UploadInfo.State.WAITING -> {
                        Text("Waiting to upload")
                    }

                    else -> {
                        val sent = "${formatSize(upload.sent)} of ${formatSize(upload.total)}"
                        Text(if (upload.total > 0) sent else "Uploading")
                        val fraction = upload.fraction
                        if (fraction != null) {
                            LinearProgressIndicator(
                                progress = { fraction },
                                modifier =
                                    Modifier
                                        .fillMaxWidth()
                                        .padding(top = 4.dp),
                            )
                        } else {
                            LinearProgressIndicator(modifier = Modifier.fillMaxWidth().padding(top = 4.dp))
                        }
                    }
                }
            }
        },
        leadingContent = {
            Icon(
                if (failed) Icons.Outlined.ErrorOutline else Icons.Outlined.CloudUpload,
                contentDescription = null,
                tint = if (failed) MaterialTheme.colorScheme.error else MaterialTheme.colorScheme.primary,
            )
        },
        trailingContent = {
            if (failed) {
                IconButton(onClick = onDismiss) { Icon(Icons.Default.Close, contentDescription = "Dismiss") }
            }
        },
    )
}
