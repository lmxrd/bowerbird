package eu.vardalabs.organizer

import android.content.Intent
import android.os.Bundle
import androidx.activity.ComponentActivity
import androidx.activity.compose.setContent
import androidx.activity.enableEdgeToEdge
import eu.vardalabs.organizer.push.EXTRA_PATH
import eu.vardalabs.organizer.ui.OrganizerRoot
import eu.vardalabs.organizer.ui.theme.OrganizerTheme

/** The app: sign in, then Files, Drop box and Settings. Receives the device login's redirect and a tapped notice. */
class MainActivity : ComponentActivity() {
    private val container get() = (application as OrganizerApplication).container

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        enableEdgeToEdge()
        if (savedInstanceState == null) handle(intent)
        setContent {
            OrganizerTheme {
                OrganizerRoot(container)
            }
        }
    }

    override fun onNewIntent(intent: Intent) {
        super.onNewIntent(intent)
        handle(intent)
    }

    private fun handle(intent: Intent?) {
        intent?.getStringExtra(EXTRA_PATH)?.let { container.opened.value = it }
        val data = intent?.data ?: return
        if (data.scheme == "organizer" && data.host == "auth") container.redirects.value = data.toString()
    }
}
