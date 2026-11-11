package eu.vardalabs.organizer.ui.dropbox

import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.FlowRow
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyListScope
import androidx.compose.foundation.lazy.items
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Add
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.Link
import androidx.compose.material.icons.outlined.Lock
import androidx.compose.material.icons.outlined.UploadFile
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.AssistChip
import androidx.compose.material3.Button
import androidx.compose.material3.Card
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.DropdownMenu
import androidx.compose.material3.DropdownMenuItem
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FloatingActionButton
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.ListItemDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
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
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import eu.vardalabs.organizer.api.models.Drive
import eu.vardalabs.organizer.api.models.ItemKind
import eu.vardalabs.organizer.api.models.ItemView
import eu.vardalabs.organizer.api.models.RuleAction
import eu.vardalabs.organizer.api.models.Status
import eu.vardalabs.organizer.data.FilesRepository
import eu.vardalabs.organizer.ui.common.UploadRow
import eu.vardalabs.organizer.ui.common.percent
import eu.vardalabs.organizer.ui.folders.FolderPicker

private sealed interface DropBoxDialog {
    data class Elsewhere(
        val item: ItemView,
    ) : DropBoxDialog

    data class Place(
        val item: ItemView,
    ) : DropBoxDialog

    data object Link : DropBoxDialog
}

/** The status as the web labels it. */
private fun label(status: Status) =
    when (status) {
        Status.QUEUED -> "Queued"
        Status.ANALYSING -> "Analysing"
        Status.WAITING -> "Waiting for the AI"
        Status.PROPOSED -> "Proposed"
        Status.CONFIRMED -> "Confirmed"
        Status.CHANGED -> "Changed"
        Status.REJECTED -> "Rejected"
        Status.AUTO_ACCEPTED -> "Accepted"
        Status.FAILED -> "Failed"
        Status.NEEDS_MANUAL -> "Place by hand"
        Status.UNDONE -> "Undone"
    }

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun DropBoxScreen(
    viewModel: DropBoxViewModel,
    files: FilesRepository,
    /** The drives the pickers start from: the drop box places into each the user may write in. */
    drives: suspend () -> List<Drive>,
) {
    val state by viewModel.ui.collectAsStateWithLifecycle()
    val snackbar = remember { SnackbarHostState() }
    var dialog by remember { mutableStateOf<DropBoxDialog?>(null) }
    var adding by remember { mutableStateOf(false) }
    val pick =
        rememberLauncherForActivityResult(ActivityResultContracts.OpenMultipleDocuments()) { viewModel.addFiles(it) }

    LaunchedEffect(state.message) {
        state.message?.let {
            snackbar.showSnackbar(it)
            viewModel.messageShown()
        }
    }

    Scaffold(
        contentWindowInsets = WindowInsets(0, 0, 0, 0),
        topBar = { TopAppBar(title = { Text("Drop box") }) },
        floatingActionButton = {
            Box {
                FloatingActionButton(
                    onClick = { adding = true },
                ) { Icon(Icons.Default.Add, contentDescription = "Add") }
                DropdownMenu(expanded = adding, onDismissRequest = { adding = false }) {
                    DropdownMenuItem(
                        text = { Text("Add files") },
                        leadingIcon = { Icon(Icons.Outlined.UploadFile, contentDescription = null) },
                        onClick = {
                            adding = false
                            pick.launch(arrayOf("*/*"))
                        },
                    )
                    DropdownMenuItem(
                        text = { Text("Add a link") },
                        leadingIcon = { Icon(Icons.Outlined.Link, contentDescription = null) },
                        onClick = {
                            adding = false
                            dialog = DropBoxDialog.Link
                        },
                    )
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
            val sections = state.sections
            val empty =
                sections.toReview.isEmpty() && sections.needsYou.isEmpty() &&
                    sections.analysing.isEmpty() && sections.placed.isEmpty() && state.uploads.isEmpty()
            when {
                state.loading -> {
                    CircularProgressIndicator(modifier = Modifier.align(Alignment.Center))
                }

                state.error != null && empty -> {
                    Text(state.error!!, modifier = Modifier.align(Alignment.Center).padding(24.dp))
                }

                empty -> {
                    Text(
                        "The drop box is empty. Share files or links into Organizer from any app, or add them here.",
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                        modifier = Modifier.align(Alignment.Center).padding(24.dp),
                    )
                }

                else -> {
                    LazyColumn(
                        modifier = Modifier.fillMaxSize(),
                        verticalArrangement = Arrangement.spacedBy(8.dp),
                        contentPadding =
                            androidx.compose.foundation.layout
                                .PaddingValues(16.dp),
                    ) {
                        items(state.uploads, key = { "upload-${it.id}" }) {
                            UploadRow(it, onDismiss = { viewModel.dismissUpload(it.id) })
                        }
                        section("To review", sections.toReview) { item ->
                            ItemCard(item, state.busy == item.id) {
                                Button(onClick = { viewModel.confirm(item) }) { Text("Confirm") }
                                OutlinedButton(
                                    onClick = { dialog = DropBoxDialog.Elsewhere(item) },
                                ) { Text("Elsewhere") }
                                val placement = item.placement
                                if (placement != null) {
                                    OutlinedButton(onClick = { viewModel.undo(item) }) { Text("Undo") }
                                } else {
                                    OutlinedButton(onClick = { viewModel.reject(item) }) { Text("Reject") }
                                }
                            }
                        }
                        section("Waiting for you", sections.needsYou) { item ->
                            ItemCard(item, state.busy == item.id) {
                                Button(onClick = { dialog = DropBoxDialog.Place(item) }) { Text("Place") }
                                OutlinedButton(onClick = { viewModel.retry(item) }) { Text("Retry") }
                            }
                        }
                        section("Analysing…", sections.analysing) { item ->
                            ItemCard(item, busy = true) {}
                        }
                        section("Placed", sections.placed) { item ->
                            ItemCard(item, state.busy == item.id) {
                                if (item.placement != null) {
                                    OutlinedButton(onClick = { viewModel.undo(item) }) { Text("Undo") }
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    when (val shown = dialog) {
        null -> {}

        is DropBoxDialog.Elsewhere -> {
            FolderPicker(
                files = files,
                title = "Where ${shown.item.name} goes",
                action = "Confirm here",
                onPick = {
                    viewModel.elsewhere(shown.item, it)
                    dialog = null
                },
                onDismiss = { dialog = null },
                allowRoot = false,
                drives = drives,
            )
        }

        is DropBoxDialog.Place -> {
            FolderPicker(
                files = files,
                title = "Place ${shown.item.name}",
                action = "Place here",
                onPick = {
                    viewModel.place(shown.item, it)
                    dialog = null
                },
                onDismiss = { dialog = null },
                allowRoot = false,
                drives = drives,
            )
        }

        DropBoxDialog.Link -> {
            LinkDialog(onDismiss = { dialog = null }) { url, note ->
                viewModel.addLink(url, note)
                dialog = null
            }
        }
    }
}

private fun LazyListScope.section(
    title: String,
    items: List<ItemView>,
    row: @Composable (ItemView) -> Unit,
) {
    if (items.isEmpty()) return
    item(key = "header-$title") {
        Text(
            "$title (${items.size})",
            style = MaterialTheme.typography.titleSmall,
            color = MaterialTheme.colorScheme.primary,
            modifier = Modifier.padding(top = 8.dp),
        )
    }
    items(items, key = { it.id }) { row(it) }
}

@Composable
private fun ItemCard(
    item: ItemView,
    busy: Boolean,
    actions: @Composable () -> Unit,
) {
    val proposal = item.proposal
    val target = item.placement?.path ?: proposal?.path
    Card(modifier = Modifier.fillMaxWidth()) {
        ListItem(
            headlineContent = { Text(item.name, maxLines = 2, overflow = TextOverflow.Ellipsis) },
            colors = ListItemDefaults.colors(containerColor = Color.Transparent),
            overlineContent = { Text(label(item.status)) },
            supportingContent = {
                Column(verticalArrangement = Arrangement.spacedBy(2.dp)) {
                    if (target != null) {
                        Text("→ $target", style = MaterialTheme.typography.bodyMedium)
                    }
                    proposal?.reason?.let { Text(it, style = MaterialTheme.typography.bodySmall) }
                    item.note?.let { Text("Note: $it", style = MaterialTheme.typography.bodySmall) }
                    if (item.status in setOf(Status.FAILED, Status.NEEDS_MANUAL, Status.WAITING)) {
                        item.error?.let {
                            Text(
                                it,
                                color = MaterialTheme.colorScheme.error,
                                style = MaterialTheme.typography.bodySmall,
                            )
                        }
                    }
                }
            },
            leadingContent = {
                Icon(
                    when {
                        item.guard == RuleAction.BY_HAND -> Icons.Outlined.Lock
                        item.kind == ItemKind.LINK -> Icons.Outlined.Link
                        item.kind == ItemKind.FOLDER -> Icons.Outlined.Folder
                        else -> Icons.Outlined.Description
                    },
                    contentDescription = null,
                )
            },
            trailingContent = {
                when {
                    busy -> {
                        CircularProgressIndicator(modifier = Modifier.size(20.dp), strokeWidth = 2.dp)
                    }

                    proposal != null && item.status == Status.PROPOSED -> {
                        AssistChip(onClick = {}, label = { Text(percent(proposal.confidence)) })
                    }
                }
            },
        )
        FlowRow(
            horizontalArrangement = Arrangement.spacedBy(8.dp),
            modifier = Modifier.padding(start = 16.dp, end = 16.dp, bottom = 12.dp),
        ) { actions() }
    }
}

@Composable
private fun LinkDialog(
    onDismiss: () -> Unit,
    onAdd: (String, String?) -> Unit,
) {
    var url by remember { mutableStateOf("") }
    var note by remember { mutableStateOf("") }
    val valid = url.trim().let { it.startsWith("http://") || it.startsWith("https://") }
    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text("Add a link") },
        text = {
            Column(verticalArrangement = Arrangement.spacedBy(8.dp)) {
                OutlinedTextField(
                    value = url,
                    onValueChange = { url = it },
                    label = { Text("https://…") },
                    singleLine = true,
                )
                OutlinedTextField(
                    value = note,
                    onValueChange = { note = it.take(2_000) },
                    label = { Text("Note (optional)") },
                )
            }
        },
        confirmButton = { TextButton(onClick = { onAdd(url, note) }, enabled = valid) { Text("Add") } },
        dismissButton = { TextButton(onClick = onDismiss) { Text("Cancel") } },
    )
}
