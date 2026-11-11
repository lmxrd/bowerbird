package eu.vardalabs.organizer.share

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.imePadding
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Close
import androidx.compose.material.icons.outlined.Description
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.Link
import androidx.compose.material.icons.outlined.Notes
import androidx.compose.material3.Button
import androidx.compose.material3.CircularProgressIndicator
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.FilterChip
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.OutlinedButton
import androidx.compose.material3.OutlinedTextField
import androidx.compose.material3.Scaffold
import androidx.compose.material3.SegmentedButton
import androidx.compose.material3.SegmentedButtonDefaults
import androidx.compose.material3.SingleChoiceSegmentedButtonRow
import androidx.compose.material3.Surface
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.dp
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import eu.vardalabs.organizer.api.models.Drive
import eu.vardalabs.organizer.api.models.Links
import eu.vardalabs.organizer.data.FilesRepository
import eu.vardalabs.organizer.ui.folders.FolderPicker

private val linkModeLabels =
    mapOf(
        Links.BOOKMARK to "Bookmark",
        Links.SNAPSHOT to "Snapshot",
        Links.DOWNLOAD to "Download",
    )

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun ShareScreen(
    items: List<SharedItem>,
    viewModel: ShareViewModel,
    files: FilesRepository,
    /** The drives the picker starts from (members may write in a shared drive). */
    drives: suspend () -> List<Drive>,
    onClose: () -> Unit,
) {
    val state by viewModel.ui.collectAsStateWithLifecycle()
    var picking by remember { mutableStateOf(false) }

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text("Save to Organizer") },
                navigationIcon = {
                    IconButton(onClick = onClose) { Icon(Icons.Default.Close, contentDescription = "Cancel") }
                },
            )
        },
        bottomBar = {
            Surface(tonalElevation = 3.dp) {
                Column(modifier = Modifier.padding(16.dp)) {
                    state.error?.let {
                        Text(it, color = MaterialTheme.colorScheme.error, modifier = Modifier.padding(bottom = 8.dp))
                    }
                    Button(
                        onClick = viewModel::onSend,
                        enabled = state.canSend && items.isNotEmpty(),
                        modifier = Modifier.fillMaxWidth(),
                    ) {
                        if (state.sending) {
                            CircularProgressIndicator(modifier = Modifier.size(20.dp), strokeWidth = 2.dp)
                        } else {
                            Text("Send")
                        }
                    }
                }
            }
        },
    ) { padding ->
        Column(
            modifier =
                Modifier
                    .fillMaxSize()
                    .padding(padding)
                    .imePadding()
                    .verticalScroll(rememberScrollState())
                    .padding(horizontal = 16.dp),
            verticalArrangement = Arrangement.spacedBy(12.dp),
        ) {
            if (items.isEmpty()) {
                Text("Nothing to save: the app shared nothing Organizer can take.")
                return@Column
            }
            items.take(MAX_SHOWN).forEach { item ->
                ListItem(
                    headlineContent = { Text(item.label, maxLines = 1, overflow = TextOverflow.Ellipsis) },
                    leadingContent = {
                        Icon(
                            when (item) {
                                is SharedItem.File -> Icons.Outlined.Description
                                is SharedItem.Link -> Icons.Outlined.Link
                                is SharedItem.Text -> Icons.Outlined.Notes
                            },
                            contentDescription = null,
                        )
                    },
                )
            }
            if (items.size > MAX_SHOWN) Text("and ${items.size - MAX_SHOWN} more")

            SingleChoiceSegmentedButtonRow(modifier = Modifier.fillMaxWidth()) {
                ShareMode.entries.forEachIndexed { index, mode ->
                    SegmentedButton(
                        selected = state.mode == mode,
                        onClick = { viewModel.onMode(mode) },
                        shape = SegmentedButtonDefaults.itemShape(index, ShareMode.entries.size),
                    ) {
                        Text(if (mode == ShareMode.DROP_BOX) "Drop box" else "Place manually")
                    }
                }
            }

            when (state.mode) {
                ShareMode.DROP_BOX -> {
                    Text(
                        "The drop box files it for you: it proposes a place, which you confirm.",
                        style = MaterialTheme.typography.bodyMedium,
                        color = MaterialTheme.colorScheme.onSurfaceVariant,
                    )
                    OutlinedTextField(
                        value = state.note,
                        onValueChange = viewModel::onNote,
                        label = { Text("Note (optional)") },
                        placeholder = { Text("What it is, where it should go") },
                        minLines = 2,
                        modifier = Modifier.fillMaxWidth(),
                    )
                    if (state.offerReadContents) {
                        Row(verticalAlignment = Alignment.CenterVertically) {
                            Column(modifier = Modifier.weight(1f)) {
                                Text("Read the contents")
                                Text(
                                    "Its text may be read and sent to the AI",
                                    style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                                )
                            }
                            Switch(checked = state.readContents, onCheckedChange = viewModel::onReadContents)
                        }
                    }
                    if (state.offerLinkMode) {
                        Text("The link becomes", style = MaterialTheme.typography.labelLarge)
                        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                            Links.entries.forEach { mode ->
                                FilterChip(
                                    selected = state.linkMode == mode,
                                    onClick = { viewModel.onLinkMode(mode) },
                                    label = { Text(linkModeLabels.getValue(mode)) },
                                )
                            }
                        }
                    }
                }

                ShareMode.FOLDER -> {
                    ListItem(
                        headlineContent = { Text(state.folder?.location ?: "No folder chosen") },
                        supportingContent = { Text("Folder") },
                        leadingContent = { Icon(Icons.Outlined.Folder, contentDescription = null) },
                        trailingContent = {
                            OutlinedButton(onClick = { picking = true }) { Text("Choose") }
                        },
                    )
                }
            }
        }
    }

    if (picking) {
        FolderPicker(
            files = files,
            title = "Place in",
            action = "Choose this folder",
            onPick = {
                viewModel.onFolder(it)
                picking = false
            },
            onDismiss = { picking = false },
            drives = drives,
        )
    }
}

@Composable
fun NotSignedIn(
    onOpen: () -> Unit,
    onClose: () -> Unit,
) {
    Scaffold { padding ->
        Column(
            modifier =
                Modifier
                    .fillMaxSize()
                    .padding(padding)
                    .padding(24.dp),
            verticalArrangement = Arrangement.spacedBy(16.dp, Alignment.CenterVertically),
            horizontalAlignment = Alignment.CenterHorizontally,
        ) {
            Text("Sign in first", style = MaterialTheme.typography.headlineSmall)
            Text("Open Organizer and sign in to your server, then share again.")
            Button(onClick = onOpen) { Text("Open Organizer") }
            OutlinedButton(onClick = onClose) { Text("Cancel") }
        }
    }
}

private const val MAX_SHOWN = 5
