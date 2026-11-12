package eu.vardalabs.organizer.ui.folders

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.outlined.Backup
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.FolderShared
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.Public
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.rememberCoroutineScope
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.compose.ui.window.Dialog
import androidx.compose.ui.window.DialogProperties
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import eu.vardalabs.organizer.api.models.Drive
import eu.vardalabs.organizer.api.models.DriveKind
import eu.vardalabs.organizer.data.FilesRepository
import eu.vardalabs.organizer.data.Folder
import eu.vardalabs.organizer.data.mayAdd
import java.util.UUID

/**
 * A full-screen folder picker: browse the drives and their folders, then
 * choose the folder on screen (the logic is [FolderPickerModel]). With
 * [drives] and no [start] it starts at the list of drives.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FolderPicker(
    files: FilesRepository,
    title: String,
    action: String,
    onPick: (Folder) -> Unit,
    onDismiss: () -> Unit,
    start: UUID? = null,
    exclude: UUID? = null,
    /** Whether a drive's root may be chosen (the drop box places items in folders only). */
    allowRoot: Boolean = true,
    /** The drives to start from; null to stay in the tree of [start]. */
    drives: (suspend () -> List<Drive>)? = null,
) {
    val scope = rememberCoroutineScope()
    val model = remember { FolderPickerModel(files, drives, start, exclude, allowRoot, scope) }
    val state by model.ui.collectAsStateWithLifecycle()
    val folder = state.folder

    Dialog(onDismissRequest = onDismiss, properties = DialogProperties(usePlatformDefaultWidth = false)) {
        Surface(modifier = Modifier.fillMaxSize()) {
            Scaffold(
                topBar = {
                    TopAppBar(
                        title = {
                            Column {
                                Text(title)
                                Text(
                                    folder?.location ?: if (state.drives != null) "Drives" else "",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    maxLines = 1,
                                    overflow = TextOverflow.Ellipsis,
                                )
                            }
                        },
                        navigationIcon = {
                            if (state.canGoUp) {
                                IconButton(onClick = model::up) {
                                    Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Up")
                                }
                            } else {
                                IconButton(onClick = onDismiss) {
                                    Icon(Icons.Default.Close, contentDescription = "Cancel")
                                }
                            }
                        },
                    )
                },
                bottomBar = {
                    Surface(tonalElevation = 3.dp) {
                        Column {
                            if (folder != null && state.viewOnly && !state.loading) {
                                Text(
                                    "You can only view this folder",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    modifier = Modifier.padding(start = 16.dp, top = 12.dp, end = 16.dp),
                                )
                            }
                            Button(
                                onClick = { folder?.let(onPick) },
                                enabled = folder != null && state.canPick,
                                modifier =
                                    Modifier
                                        .fillMaxWidth()
                                        .padding(16.dp),
                            ) {
                                Text(action)
                            }
                        }
                    }
                },
            ) { padding ->
                Box(
                    modifier =
                        Modifier
                            .fillMaxSize()
                            .padding(padding),
                ) {
                    val drivesList = state.drives
                    when {
                        state.error != null -> {
                            Text(state.error!!, modifier = Modifier.align(Alignment.Center).padding(24.dp))
                        }

                        state.loading -> {
                            CircularProgressIndicator(modifier = Modifier.align(Alignment.Center))
                        }

                        drivesList != null -> {
                            LazyColumn {
                                items(drivesList, key = { it.id }) { drive ->
                                    ListItem(
                                        headlineContent = { Text(drive.name) },
                                        leadingContent = { DriveIcon(drive) },
                                        modifier = Modifier.clickable { model.openDrive(drive) },
                                    )
                                    HorizontalDivider()
                                }
                            }
                        }

                        state.children.isEmpty() -> {
                            Text(
                                "No folders in here",
                                modifier = Modifier.align(Alignment.Center),
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }

                        else -> {
                            LazyColumn {
                                items(state.children, key = { it.id }) { child ->
                                    ListItem(
                                        headlineContent = { Text(child.name) },
                                        supportingContent =
                                            if (child.myRole.mayAdd) {
                                                null
                                            } else {
                                                { Text("View only") }
                                            },
                                        leadingContent = {
                                            if (child.e2eRoot != null) {
                                                Icon(Icons.Outlined.Lock, contentDescription = "Encrypted")
                                            } else {
                                                Icon(Icons.Outlined.Folder, contentDescription = null)
                                            }
                                        },
                                        modifier = Modifier.clickable { model.open(child.id) },
                                    )
                                    HorizontalDivider()
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

/** My drive, or a shared drive (one of several people). */
@Composable
fun DriveIcon(drive: Drive) {
    Icon(
        when (drive.kind) {
            DriveKind.MY -> Icons.Outlined.Folder
            DriveKind.MIRROR -> Icons.Outlined.Backup
            DriveKind.REMOTE -> Icons.Outlined.Public
            else -> Icons.Outlined.FolderShared
        },
        contentDescription = null,
        tint = MaterialTheme.colorScheme.primary,
    )
}
