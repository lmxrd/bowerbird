package eu.vardalabs.organizer.ui.settings

import android.content.ActivityNotFoundException
import androidx.activity.compose.LocalActivity
import androidx.browser.customtabs.CustomTabsIntent
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.outlined.OpenInNew
import androidx.compose.material.icons.outlined.AccountCircle
import androidx.compose.material.icons.outlined.Dns
import androidx.compose.material.icons.outlined.Key
import androidx.compose.material.icons.outlined.Notifications
import androidx.compose.material.icons.outlined.PhoneAndroid
import androidx.compose.material3.AlertDialog
import androidx.compose.material3.Button
import androidx.compose.material3.ButtonDefaults
import androidx.compose.material3.ExperimentalMaterial3Api
import androidx.compose.material3.HorizontalDivider
import androidx.compose.material3.Icon
import androidx.compose.material3.ListItem
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Switch
import androidx.compose.material3.Text
import androidx.compose.material3.TextButton
import androidx.compose.material3.TopAppBar
import androidx.compose.runtime.Composable
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.unit.dp
import androidx.core.net.toUri
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import eu.vardalabs.organizer.push.pickDistributor

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun SettingsScreen(viewModel: SettingsViewModel) {
    val state by viewModel.ui.collectAsStateWithLifecycle()
    val context = LocalContext.current
    val activity = LocalActivity.current
    var confirming by remember { mutableStateOf(false) }
    val openWeb = {
        try {
            CustomTabsIntent.Builder().build().launchUrl(context, "${state.server}/settings".toUri())
        } catch (_: ActivityNotFoundException) {
        }
    }

    Scaffold(
        contentWindowInsets = WindowInsets(0, 0, 0, 0),
        topBar = { TopAppBar(title = { Text("Settings") }) },
    ) { padding ->
        Column(
            modifier =
                Modifier
                    .fillMaxSize()
                    .padding(padding)
                    .verticalScroll(rememberScrollState()),
        ) {
            ListItem(
                headlineContent = { Text(state.server) },
                overlineContent = { Text("Server") },
                leadingContent = { Icon(Icons.Outlined.Dns, contentDescription = null) },
            )
            ListItem(
                headlineContent = { Text(state.email ?: "…") },
                overlineContent = { Text("Signed in as") },
                leadingContent = { Icon(Icons.Outlined.AccountCircle, contentDescription = null) },
            )
            ListItem(
                headlineContent = { Text(state.deviceName) },
                overlineContent = { Text("This device") },
                supportingContent = { Text("Listed on the web under Settings → Devices") },
                leadingContent = { Icon(Icons.Outlined.PhoneAndroid, contentDescription = null) },
            )
            ListItem(
                headlineContent = { Text("${state.keyPrefix}…") },
                overlineContent = { Text("Signed in with the token") },
                leadingContent = { Icon(Icons.Outlined.Key, contentDescription = null) },
            )
            val push = state.notifications
            ListItem(
                headlineContent = { Text("Notifications") },
                supportingContent = {
                    Text(
                        when {
                            push.on -> "Through ${push.distributor}"
                            push.missing -> "No UnifiedPush distributor found: install one, such as ntfy"
                            else -> "Through a UnifiedPush distributor, such as ntfy"
                        },
                    )
                },
                leadingContent = { Icon(Icons.Outlined.Notifications, contentDescription = null) },
                trailingContent = {
                    Switch(
                        checked = push.on,
                        enabled = !state.switching,
                        onCheckedChange = { on ->
                            when {
                                !on -> viewModel.disableNotifications()
                                activity != null -> pickDistributor(activity) { viewModel.enableNotifications(it) }
                            }
                        },
                    )
                },
            )
            HorizontalDivider()
            ListItem(
                headlineContent = { Text("Other settings are on the web") },
                supportingContent = {
                    Text("Your account, two-factor, devices, the drop box and the AI: open Organizer in the browser.")
                },
                trailingContent = { Icon(Icons.AutoMirrored.Outlined.OpenInNew, contentDescription = null) },
                modifier = Modifier.clickable { openWeb() },
            )
            HorizontalDivider()
            Button(
                onClick = { confirming = true },
                enabled = !state.signingOut,
                colors = ButtonDefaults.buttonColors(containerColor = MaterialTheme.colorScheme.error),
                modifier =
                    Modifier
                        .fillMaxWidth()
                        .padding(16.dp),
            ) {
                Text(if (state.signingOut) "Signing out…" else "Sign out")
            }
        }
    }

    if (confirming) {
        AlertDialog(
            onDismissRequest = { confirming = false },
            title = { Text("Sign out?") },
            text = {
                Text(
                    "This device's token is revoked, and uploads that wait are dropped.",
                )
            },
            confirmButton = {
                TextButton(onClick = {
                    confirming = false
                    viewModel.signOut()
                }) { Text("Sign out") }
            },
            dismissButton = { TextButton(onClick = { confirming = false }) { Text("Cancel") } },
        )
    }
}
