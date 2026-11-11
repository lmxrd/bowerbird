package eu.vardalabs.organizer.ui.common

import android.content.ActivityNotFoundException
import android.content.Intent
import androidx.compose.material3.SnackbarHostState
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.platform.LocalContext
import androidx.core.content.FileProvider
import eu.vardalabs.organizer.ui.files.OpenFile

/** Opens a downloaded file in another app (a chooser), once; says so when no app can. */
@Composable
fun OpenFileEffect(
    toOpen: OpenFile?,
    snackbar: SnackbarHostState,
    onOpened: () -> Unit,
) {
    val context = LocalContext.current
    LaunchedEffect(toOpen) {
        val open = toOpen ?: return@LaunchedEffect
        val uri = FileProvider.getUriForFile(context, "${context.packageName}.files", open.file)
        val intent =
            Intent(Intent.ACTION_VIEW)
                .setDataAndType(uri, open.mime ?: "application/octet-stream")
                .addFlags(Intent.FLAG_GRANT_READ_URI_PERMISSION)
        try {
            context.startActivity(Intent.createChooser(intent, open.file.name))
        } catch (_: ActivityNotFoundException) {
            snackbar.showSnackbar("No app can open ${open.file.name}")
        }
        onOpened()
    }
}
