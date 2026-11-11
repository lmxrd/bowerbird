package eu.vardalabs.organizer.ui.files

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.text.KeyboardActions
import androidx.compose.foundation.text.KeyboardOptions
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.outlined.CreateNewFolder
import androidx.compose.material.icons.outlined.Delete
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.DriveFileMove
import androidx.compose.material.icons.outlined.Edit
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.MoreVert
import androidx.compose.material.icons.outlined.SyncAlt
import androidx.compose.material.icons.outlined.UploadFile
import androidx.compose.material.icons.outlined.WarningAmber
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SnackbarHost
import androidx.compose.material3.SnackbarHostState
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.material3.pulltorefresh.PullToRefreshBox
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.text.input.ImeAction
import androidx.compose.ui.text.input.KeyboardType
import androidx.compose.ui.text.input.PasswordVisualTransformation
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import eu.vardalabs.organizer.api.models.Kind
import eu.vardalabs.organizer.api.models.Node
import eu.vardalabs.organizer.data.FilesRepository
import eu.vardalabs.organizer.data.movingNote
import eu.vardalabs.organizer.ui.common.OpenFileEffect
import eu.vardalabs.organizer.ui.common.UploadRow
import eu.vardalabs.organizer.ui.common.formatDate
import eu.vardalabs.organizer.ui.common.formatSize
import eu.vardalabs.organizer.ui.folders.FolderPicker
import java.util.UUID

/** What a dialog of this screen is about. */
private sealed interface FilesDialog {
    data object NewFolder : FilesDialog

    data class Rename(
        val node: Node,
    ) : FilesDialog

    data class Move(
        val node: Node,
    ) : FilesDialog

    data class Delete(
        val node: Node,
    ) : FilesDialog
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun FilesScreen(
    viewModel: FilesViewModel,
    files: FilesRepository,
    onOpenFolder: (UUID) -> Unit,
    onBack: (() -> Unit)?,
) {
    val state by viewModel.ui.collectAsStateWithLifecycle()
    val snackbar = remember { SnackbarHostState() }
    var dialog by remember { mutableStateOf<FilesDialog?>(null) }
    var adding by remember { mutableStateOf(false) }
    // The unlock prompt shows on its own when a locked folder opens; Cancel hides it.
    var unlockDismissed by remember { mutableStateOf(false) }
    val pick =
        rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { viewModel.upload(it) }

    LaunchedEffect(state.message) {
        state.message?.let {
            snackbar.showSnackbar(it)
            viewModel.messageShown()
        }
    }
    OpenFileEffect(state.toOpen, snackbar, viewModel::opened)

    Scaffold(
        contentWindowInsets = WindowInsets(0, 0, 0, 0),
        topBar = {
            TopAppBar(
                title = {
                    val folder = state.folder
                    Column {
                        Text(folder?.name ?: "Files", maxLines = 1, overflow = TextOverflow.Ellipsis)
                        // Below a drive's root: where it is, from the drive (`Team/Plans`).
                        if (folder != null && folder.parentId != null && folder.location != folder.name) {
                            Text(
                                folder.location,
                                style = MaterialTheme.typography.bodySmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                                maxLines = 1,
                                overflow = TextOverflow.Ellipsis,
                            )
                        }
                    }
                },
                navigationIcon = {
                    if (onBack != null) {
                        IconButton(onClick = onBack) {
                            Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = "Back")
                        }
                    }
                },
            )
        },
        floatingActionButton = {
            // A viewer adds nothing; upload only and up may.
            if (state.mayAdd) {
                Box {
                    FloatingActionButton(onClick = { adding = true }) {
                        Icon(Icons.Default.Add, contentDescription = "Add")
                    }
                    DropdownMenu(expanded = adding, onDismissRequest = { adding = false }) {
                        DropdownMenuItem(
                            text = { Text("Upload files") },
                            leadingIcon = { Icon(Icons.Outlined.UploadFile, contentDescription = null) },
                            onClick = {
                                adding = false
                                pick.launch(arrayOf("*/*"))
                            },
                        )
                        DropdownMenuItem(
                            text = { Text("New folder") },
                            leadingIcon = { Icon(Icons.Outlined.CreateNewFolder, contentDescription = null) },
                            onClick = {
                                adding = false
                                dialog = FilesDialog.NewFolder
                            },
                        )
                    }
                }
            }
        },
        snackbarHost = { SnackbarHost(snackbar) },
    ) { padding ->
        PullToRefreshBox(
            isRefreshing = state.refreshing,
            onRefresh = viewModel::refresh,
            modifier =
                Modifier
                    .fillMaxSize()
                    .padding(padding),
        ) {
            when {
                state.loading -> {
                    CircularProgressIndicator(modifier = Modifier.align(Alignment.Center))
                }

                state.error != null && state.folder == null -> {
                    Text(state.error!!, modifier = Modifier.align(Alignment.Center).padding(24.dp))
                }

                state.locked -> {
                    Locked(onUnlock = { unlockDismissed = false }, modifier = Modifier.align(Alignment.Center))
                }

                else -> {
                    LazyColumn(modifier = Modifier.fillMaxSize()) {
                        // Where its drive is stored, when its members should know (docs/M7C_DESIGN.md).
                        state.folder?.storageWarning?.let { warning ->
                            item(key = "storage-warning") { StorageNote(warning, Icons.Outlined.WarningAmber) }
                        }
                        state.folder?.movingTo?.let { to ->
                            item(key = "moving") { StorageNote(movingNote(to), Icons.Outlined.SyncAlt) }
                        }
                        if (state.onlyOwnUploads) {
                            item(key = "only-own") {
                                Text(
                                    "You can add files here, and see only those you added.",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                                )
                            }
                        }
                        items(state.uploads, key = { "upload-${it.id}" }) { upload ->
                            UploadRow(upload, onDismiss = { viewModel.dismissUpload(upload.id) })
                        }
                        if (state.items.isEmpty() && state.uploads.isEmpty()) {
                            item {
                                Text(
                                    "This folder is empty",
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    modifier = Modifier.padding(24.dp),
                                )
                            }
                        }
                        items(state.items, key = { it.id }) { node ->
                            // What its role allows, nothing while its drive is being moved.
                            val actions = state.actions(node)
                            NodeRow(
                                node = node,
                                opening = state.opening == node.id,
                                onClick = {
                                    if (node.kind == Kind.FOLDER) onOpenFolder(node.id) else viewModel.open(node)
                                },
                                onRename = if (actions.rename) ({ dialog = FilesDialog.Rename(node) }) else null,
                                onMove = if (actions.move) ({ dialog = FilesDialog.Move(node) }) else null,
                                onDelete = if (actions.delete) ({ dialog = FilesDialog.Delete(node) }) else null,
                            )
                            HorizontalDivider()
                        }
                    }
                }
            }
        }
    }

    if (state.locked && !unlockDismissed) {
        UnlockDialog(
            unlocking = state.unlocking,
            error = state.unlockError,
            onUnlock = viewModel::unlock,
            onDismiss = { unlockDismissed = true },
        )
    }

    when (val shown = dialog) {
        null -> {}

        FilesDialog.NewFolder -> {
            NameDialog("New folder", "", "Create", onDismiss = { dialog = null }) {
                viewModel.createFolder(it)
                dialog = null
            }
        }

        is FilesDialog.Rename -> {
            NameDialog("Rename", shown.node.name, "Rename", onDismiss = { dialog = null }) {
                viewModel.rename(shown.node, it)
                dialog = null
            }
        }

        is FilesDialog.Move -> {
            FolderPicker(
                files = files,
                title = "Move ${shown.node.name}",
                action = "Move here",
                start = shown.node.parentId,
                exclude = shown.node.id,
                onPick = {
                    viewModel.move(shown.node, it)
                    dialog = null
                },
                onDismiss = { dialog = null },
            )
        }

        is FilesDialog.Delete -> {
            AlertDialog(
                onDismissRequest = { dialog = null },
                title = { Text("Delete ${shown.node.name}?") },
                text = { Text("It goes to the trash, where it can be restored on the web.") },
                confirmButton = {
                    TextButton(onClick = {
                        viewModel.delete(shown.node)
                        dialog = null
                    }) { Text("Delete") }
                },
                dismissButton = { TextButton(onClick = { dialog = null }) { Text("Cancel") } },
            )
        }
    }
}

@Composable
private fun NodeRow(
    node: Node,
    opening: Boolean,
    onClick: () -> Unit,
    onRename: (() -> Unit)?,
    onMove: (() -> Unit)?,
    onDelete: (() -> Unit)?,
) {
    var menu by remember { mutableStateOf(false) }
    val folder = node.kind == Kind.FOLDER
    ListItem(
        headlineContent = { Text(node.name, maxLines = 1, overflow = TextOverflow.Ellipsis) },
        supportingContent = {
            Text(
                if (folder) {
                    formatDate(
                        node.updatedAt,
                    )
                } else {
                    "${formatSize(node.propertySize)} · ${formatDate(node.updatedAt)}"
                },
            )
        },
        leadingContent = {
            when {
                opening -> {
                    CircularProgressIndicator(modifier = Modifier.size(24.dp), strokeWidth = 2.dp)
                }

                folder -> {
                    Icon(
                        if (node.e2eRoot != null) Icons.Outlined.Lock else Icons.Outlined.Folder,
                        contentDescription = if (node.e2eRoot != null) "Encrypted" else null,
                        tint = MaterialTheme.colorScheme.primary,
                    )
                }

                else -> {
                    Icon(Icons.Outlined.Description, contentDescription = null)
                }
            }
        },
        trailingContent = {
            // A viewer's items have no menu.
            if (onRename == null && onMove == null && onDelete == null) return@ListItem
            Box {
                IconButton(onClick = { menu = true }) { Icon(Icons.Outlined.MoreVert, contentDescription = "More") }
                DropdownMenu(expanded = menu, onDismissRequest = { menu = false }) {
                    if (onRename != null) {
                        DropdownMenuItem(
                            text = { Text("Rename") },
                            leadingIcon = { Icon(Icons.Outlined.Edit, contentDescription = null) },
                            onClick = {
                                menu = false
                                onRename()
                            },
                        )
                    }
                    if (onMove != null) {
                        DropdownMenuItem(
                            text = { Text("Move") },
                            leadingIcon = { Icon(Icons.Outlined.DriveFileMove, contentDescription = null) },
                            onClick = {
                                menu = false
                                onMove()
                            },
                        )
                    }
                    if (onDelete != null) {
                        DropdownMenuItem(
                            text = { Text("Delete") },
                            leadingIcon = { Icon(Icons.Outlined.Delete, contentDescription = null) },
                            onClick = {
                                menu = false
                                onDelete()
                            },
                        )
                    }
                }
            }
        },
        modifier = Modifier.clickable(onClick = onClick),
    )
}

/** What a locked encrypted folder shows behind the unlock prompt. */
@Composable
private fun Locked(
    onUnlock: () -> Unit,
    modifier: Modifier = Modifier,
) {
    Column(modifier = modifier.padding(24.dp), horizontalAlignment = Alignment.CenterHorizontally) {
        Icon(Icons.Outlined.Lock, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant)
        Text(
            "Unlock to see what's inside",
            style = MaterialTheme.typography.titleMedium,
            modifier = Modifier.padding(top = 12.dp),
        )
        Text(
            "Enter your encryption passphrase on this device.",
            color = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.padding(top = 4.dp, bottom = 12.dp),
        )
        Button(onClick = onUnlock) { Text("Unlock") }
    }
}

/** The passphrase that unlocks this device's keys of the encrypted folders. */
@Composable
private fun UnlockDialog(
    unlocking: Boolean,
    error: String?,
    onUnlock: (String) -> Unit,
    onDismiss: () -> Unit,
) {
    var passphrase by remember { mutableStateOf("") }
    AlertDialog(
        onDismissRequest = onDismiss,
        icon = { Icon(Icons.Outlined.Lock, contentDescription = null) },
        title = { Text("Unlock to see what's inside") },
        text = {
            Column {
                Text("Enter your encryption passphrase on this device.")
                OutlinedTextField(
                    value = passphrase,
                    onValueChange = { passphrase = it },
                    singleLine = true,
                    label = { Text("Passphrase") },
                    visualTransformation = PasswordVisualTransformation(),
                    keyboardOptions = KeyboardOptions(keyboardType = KeyboardType.Password, imeAction = ImeAction.Done),
                    keyboardActions = KeyboardActions(onDone = { onUnlock(passphrase) }),
                    isError = error != null,
                    supportingText = error?.let { { Text(it) } },
                    enabled = !unlocking,
                    modifier = Modifier.padding(top = 16.dp),
                )
            }
        },
        confirmButton = {
            TextButton(onClick = { onUnlock(passphrase) }, enabled = passphrase.isNotEmpty() && !unlocking) {
                if (unlocking) {
                    CircularProgressIndicator(modifier = Modifier.size(18.dp), strokeWidth = 2.dp)
                } else {
                    Text("Unlock")
                }
            }
        },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

@Composable
private fun NameDialog(
    title: String,
    initial: String,
    action: String,
    onDismiss: () -> Unit,
    onConfirm: (String) -> Unit,
) {
    var name by remember { mutableStateOf(initial) }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(title) },
        text = {
            Column {
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it },
                    singleLine = true,
                    label = { Text("Name") },
                )
            }
        },
        confirmButton = { TextButton(onClick = { onConfirm(name) }, enabled = name.isNotBlank()) { Text(action) } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}

/** A line on a folder about its drive's storage: someone's own storage, a move under way. */
@Composable
private fun StorageNote(
    text: String,
    icon: ImageVector,
) {
    ListItem(
        headlineContent = {
            Text(text, style = MaterialTheme.typography.bodySmall, color = MaterialTheme.colorScheme.onSurfaceVariant)
        },
        leadingContent = { Icon(icon, contentDescription = null, tint = MaterialTheme.colorScheme.onSurfaceVariant) },
    )
}
