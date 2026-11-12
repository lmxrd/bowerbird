package eu.vardalabs.organizer.share

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import eu.vardalabs.organizer.MainActivity
import eu.vardalabs.organizer.OrganizerApplication
import eu.vardalabs.organizer.core.session.SessionState
import eu.vardalabs.organizer.ui.theme.OrganizerTheme

/**
 * The share target (`ACTION_SEND`, `ACTION_SEND_MULTIPLE`): files, photos,
 * text and links from any app go to the drop box or a folder. It hands them
 * to the uploads and finishes at once; the uploads go on without it.
 */
class ShareActivity : ComponentActivity() {
    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        val container = (application as OrganizerApplication).container
        val items = SharedItem.from(intent) { container.staging.nameOf(it) }
        val sender =
            ShareSender(container.staging, container.encryptedUploads::requests) { container.uploads.enqueue(it) }
        setContent {
            OrganizerTheme {
                val session by container.sessions.sessions.collectAsStateWithLifecycle()
                LaunchedEffect(Unit) { container.sessions.load() }
                when (session) {
                    SessionState.Loading -> {}

                    SessionState.SignedOut -> {
                        NotSignedIn(
                            onOpen = {
                                startActivity(Intent(this, MainActivity::class.java))
                                finish()
                            },
                            onClose = ::finish,
                        )
                    }

                    is SessionState.SignedIn -> {
                        val viewModel =
                            viewModel {
                                ShareViewModel(
                                    hasLink = items.any { it is SharedItem.Link },
                                    settings = { container.dropBox.settings() },
                                    unlocked = { container.e2e.isUnlocked() },
                                    send = { sender.send(items, it) },
                                )
                            }
                        val state by viewModel.ui.collectAsStateWithLifecycle()
                        LaunchedEffect(state.done) { if (state.done) finish() }
                        ShareScreen(
                            items = items,
                            viewModel = viewModel,
                            files = container.files,
                            drives = container.drives::drives,
                            onClose = ::finish,
                        )
                    }
                }
            }
        }
    }
}
