package eu.vardalabs.organizer.ui.drives

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.CloudOff
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.SyncAlt
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
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
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import eu.vardalabs.organizer.api.models.Drive
import eu.vardalabs.organizer.api.models.DriveKind
import eu.vardalabs.organizer.api.models.Kind
import eu.vardalabs.organizer.api.models.SharedItem
import eu.vardalabs.organizer.data.DriveStorage
import eu.vardalabs.organizer.data.awaitsAnswer
import eu.vardalabs.organizer.data.fromLine
import eu.vardalabs.organizer.data.mirrorLine
import eu.vardalabs.organizer.data.opens
import eu.vardalabs.organizer.data.pending
import eu.vardalabs.organizer.data.stateLine
import eu.vardalabs.organizer.data.storage
import eu.vardalabs.organizer.data.storageLine
import eu.vardalabs.organizer.data.title
import eu.vardalabs.organizer.ui.common.OpenFileEffect
import eu.vardalabs.organizer.ui.common.formatSize
import eu.vardalabs.organizer.ui.folders.DriveIcon
import java.util.UUID

/** The Files tab's start: the drives, then what is shared with the user. */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DrivesScreen(
    viewModel: DrivesViewModel,
    onOpenFolder: (UUID) -> Unit,
) {
    val state by viewModel.ui.collectAsStateWithLifecycle()
    val snackbar = remember { SnackbarHostState() }

    LaunchedEffect(state.message) {
        state.message?.let {
            snackbar.showSnackbar(it)
            viewModel.messageShown()
        }
    }
    OpenFileEffect(state.toOpen, snackbar, viewModel::opened)

    Scaffold(
        contentWindowInsets = WindowInsets(0, 0, 0, 0),
        topBar = { TopAppBar(title = { Text("Files") }) },
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

                state.error != null -> {
                    Text(state.error!!, modifier = Modifier.align(Alignment.Center).padding(24.dp))
                }

                else -> {
                    LazyColumn(modifier = Modifier.fillMaxSize()) {
                        item { Heading("Drives") }
                        items(state.ownAndShared, key = { "drive-${it.id}" }) { drive ->
                            DriveRow(drive, onClick = { drive.rootId?.takeIf { drive.opens }?.let(onOpenFolder) })
                            HorizontalDivider()
                        }
                        // Read-only copies of drives in the user's own storage, browsed as a viewer's.
                        if (state.mirrors.isNotEmpty()) item { Heading("Mirrors") }
                        items(state.mirrors, key = { "mirror-${it.id}" }) { drive ->
                            DriveRow(drive, onClick = { drive.rootId?.takeIf { drive.opens }?.let(onOpenFolder) })
                            HorizontalDivider()
                        }
                        // Drives shared from other servers: whose and where, accepted or waiting.
                        if (state.remotes.isNotEmpty()) item { Heading("From other servers") }
                        items(state.remotes, key = { "remote-${it.id}" }) { drive ->
                            DriveRow(
                                drive,
                                onClick = { drive.rootId?.takeIf { drive.opens }?.let(onOpenFolder) },
                                answer = {
                                    val root = drive.rootId
                                    if (root != null && drive.remote?.pending == true) {
                                        AnswerButtons(
                                            answering = state.answering == root,
                                            onAccept = { viewModel.accept(root) },
                                            onDecline = { viewModel.decline(root) },
                                        )
                                    }
                                },
                            )
                            HorizontalDivider()
                        }
                        item { Heading("Shared with me") }
                        if (state.shared.isEmpty()) {
                            item {
                                Text(
                                    "Nothing is shared with you",
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                    modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp),
                                )
                            }
                        }
                        items(state.shared, key = { "shared-${it.node.id}" }) { shared ->
                            SharedRow(
                                shared = shared,
                                opening = state.opening == shared.node.id,
                                answering = state.answering == shared.node.id,
                                onAccept = { viewModel.accept(shared.node.id) },
                                onDecline = { viewModel.decline(shared.node.id) },
                                onClick = {
                                    if (shared.node.kind == Kind.FOLDER) {
                                        onOpenFolder(shared.node.id)
                                    } else {
                                        viewModel.open(shared.node)
                                    }
                                },
                            )
                            HorizontalDivider()
                        }
                    }
                }
            }
        }
    }
}

@Composable
private fun Heading(text: String) {
    Text(
        text,
        style = MaterialTheme.typography.titleSmall,
        color = MaterialTheme.colorScheme.primary,
        modifier = Modifier.padding(start = 16.dp, end = 16.dp, top = 16.dp, bottom = 4.dp),
    )
}

@Composable
private fun DriveRow(
    drive: Drive,
    onClick: () -> Unit,
    /** A remote drive's Accept and Decline, while it waits. */
    answer: @Composable () -> Unit = {},
) {
    // An unavailable drive (its folder, or its storage: failing, locked; one
    // from another server not accepted yet) offers nothing.
    val available = drive.opens
    val storage = drive.storage
    // A mirror's state: up to date, copying, waiting, failing, stopped.
    val mirrorLine = drive.mirrorLine
    val remote = drive.remote
    ListItem(
        headlineContent = { Text(drive.title, maxLines = 1, overflow = TextOverflow.Ellipsis) },
        supportingContent = {
            val line = drive.storageLine
            if (remote != null) {
                // Whose it is and where, then how it stands (waiting for Accept, its server failing).
                Column {
                    Text(remote.fromLine, maxLines = 1, overflow = TextOverflow.Ellipsis)
                    (remote.stateLine ?: line)?.let { Text(it, maxLines = 2, overflow = TextOverflow.Ellipsis) }
                }
            } else if (line != null) {
                Row(verticalAlignment = Alignment.CenterVertically) {
                    Icon(
                        when (storage) {
                            // A lock for a locked one.
                            is DriveStorage.Unavailable -> {
                                if (storage.locked) Icons.Outlined.Lock else Icons.Outlined.CloudOff
                            }

                            else -> {
                                Icons.Outlined.SyncAlt
                            }
                        },
                        contentDescription = null,
                        modifier = Modifier.size(16.dp).padding(end = 4.dp),
                    )
                    Text(line, maxLines = 2, overflow = TextOverflow.Ellipsis)
                }
            } else {
                Text(
                    when {
                        !available -> "Not available right now"
                        mirrorLine != null -> mirrorLine
                        drive.kind == DriveKind.SHARED -> membersAndSize(drive.members, formatSize(drive.used))
                        else -> formatSize(drive.used)
                    },
                )
            }
        },
        leadingContent = { DriveIcon(drive) },
        trailingContent = answer,
        modifier = if (available) Modifier.clickable(onClick = onClick) else Modifier,
    )
}

/** Accept and Decline of something shared from another server (or a progress while one is sent). */
@Composable
private fun AnswerButtons(
    answering: Boolean,
    onAccept: () -> Unit,
    onDecline: () -> Unit,
) {
    if (answering) {
        CircularProgressIndicator(modifier = Modifier.size(24.dp), strokeWidth = 2.dp)
        return
    }
    Row {
        TextButton(onClick = onDecline) { Text("Decline") }
        TextButton(onClick = onAccept) { Text("Accept") }
    }
}

@Composable
private fun SharedRow(
    shared: SharedItem,
    opening: Boolean,
    answering: Boolean,
    onAccept: () -> Unit,
    onDecline: () -> Unit,
    onClick: () -> Unit,
) {
    val folder = shared.node.kind == Kind.FOLDER
    // From another server and not accepted yet: nothing is listed until then.
    val waits = shared.awaitsAnswer
    ListItem(
        headlineContent = { Text(shared.node.name, maxLines = 1, overflow = TextOverflow.Ellipsis) },
        supportingContent = { Text(shared.fromLine, maxLines = 1, overflow = TextOverflow.Ellipsis) },
        trailingContent =
            if (waits) {
                { AnswerButtons(answering, onAccept, onDecline) }
            } else {
                null
            },
        leadingContent = {
            when {
                opening -> {
                    CircularProgressIndicator(modifier = Modifier.size(24.dp), strokeWidth = 2.dp)
                }

                folder -> {
                    Icon(
                        Icons.Outlined.Folder,
                        contentDescription = null,
                        tint = MaterialTheme.colorScheme.primary,
                    )
                }

                else -> {
                    Icon(Icons.Outlined.Description, contentDescription = null)
                }
            }
        },
        modifier = if (waits) Modifier else Modifier.clickable(onClick = onClick),
    )
}
