package eu.vardalabs.organizer.ui

import android.Manifest
import android.content.pm.PackageManager.PERMISSION_GRANTED
import android.os.Build
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.layout.Box
import androidx.compose.foundation.layout.WindowInsets
import androidx.compose.foundation.layout.fillMaxSize
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.outlined.Folder
import androidx.compose.material.icons.outlined.Inbox
import androidx.compose.material.icons.outlined.Settings
import androidx.compose.material3.Badge
import androidx.compose.material3.BadgedBox
import androidx.compose.material3.Icon
import androidx.compose.material3.NavigationBar
import androidx.compose.material3.NavigationBarItem
import androidx.compose.material3.Scaffold
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.runtime.getValue
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.vector.ImageVector
import androidx.compose.ui.platform.LocalContext
import androidx.core.content.ContextCompat
import androidx.lifecycle.Lifecycle
import androidx.lifecycle.compose.LocalLifecycleOwner
import androidx.lifecycle.compose.collectAsStateWithLifecycle
import androidx.lifecycle.repeatOnLifecycle
import androidx.lifecycle.viewmodel.compose.viewModel
import androidx.navigation.NavDestination.Companion.hasRoute
import androidx.navigation.NavDestination.Companion.hierarchy
import androidx.navigation.NavGraph.Companion.findStartDestination
import androidx.navigation.compose.NavHost
import androidx.navigation.compose.composable
import androidx.navigation.compose.currentBackStackEntryAsState
import androidx.navigation.compose.rememberNavController
import androidx.navigation.toRoute
import eu.vardalabs.organizer.AppContainer
import eu.vardalabs.organizer.core.events.LiveEvent
import eu.vardalabs.organizer.core.events.liveEvents
import eu.vardalabs.organizer.core.session.Session
import eu.vardalabs.organizer.core.session.SessionState
import eu.vardalabs.organizer.data.WAITING
import eu.vardalabs.organizer.ui.drives.DrivesScreen
import eu.vardalabs.organizer.ui.drives.DrivesViewModel
import eu.vardalabs.organizer.ui.dropbox.DropBoxScreen
import eu.vardalabs.organizer.ui.dropbox.DropBoxViewModel
import eu.vardalabs.organizer.ui.files.FilesScreen
import eu.vardalabs.organizer.ui.files.FilesViewModel
import eu.vardalabs.organizer.ui.settings.SettingsScreen
import eu.vardalabs.organizer.ui.settings.SettingsViewModel
import eu.vardalabs.organizer.ui.signin.SignInScreen
import eu.vardalabs.organizer.ui.signin.SignInViewModel
import kotlinx.coroutines.launch
import kotlinx.serialization.Serializable
import java.util.UUID
import kotlin.reflect.KClass

/** The routes of the signed-in app (type-safe Navigation Compose). The Files tab starts at the drives. */
@Serializable
data object DrivesRoute

/** A folder of any drive (a drive's root too), by its node id. */
@Serializable
data class FilesRoute(
    val folderId: String,
)

@Serializable
data object DropBoxRoute

@Serializable
data object SettingsRoute

private data class Tab(
    val label: String,
    val icon: ImageVector,
    val route: Any,
    /** The routes that belong to the tab: it shows selected on each. */
    val types: List<KClass<*>>,
)

private val tabs =
    listOf(
        Tab("Files", Icons.Outlined.Folder, DrivesRoute, listOf(DrivesRoute::class, FilesRoute::class)),
        Tab("Drop box", Icons.Outlined.Inbox, DropBoxRoute, listOf(DropBoxRoute::class)),
        Tab("Settings", Icons.Outlined.Settings, SettingsRoute, listOf(SettingsRoute::class)),
    )

@Composable
fun OrganizerRoot(container: AppContainer) {
    val state by container.sessions.sessions.collectAsStateWithLifecycle()
    when (val current = state) {
        SessionState.Loading -> Box(Modifier.fillMaxSize())
        SessionState.SignedOut -> SignIn(container)
        is SessionState.SignedIn -> Main(container, current.session)
    }
}

@Composable
private fun SignIn(container: AppContainer) {
    val viewModel =
        viewModel {
            SignInViewModel(
                deviceName = container.deviceName,
                start = { address, name -> container.signIn.start(address, name).toString() },
                complete = { container.signIn.complete(it) },
                lastServer = { container.sessions.lastServer() },
            )
        }
    val redirect by container.redirects.collectAsStateWithLifecycle()
    LaunchedEffect(redirect) {
        redirect?.let {
            container.redirects.value = null
            viewModel.onRedirect(it)
        }
    }
    SignInScreen(viewModel)
}

@Composable
private fun Main(
    container: AppContainer,
    session: Session,
) {
    val nav = rememberNavController()
    val backStack by nav.currentBackStackEntryAsState()
    val items by container.dropBox.items.collectAsStateWithLifecycle()
    val waiting = items.count { it.status in WAITING }
    val lifecycle = LocalLifecycleOwner.current.lifecycle

    // Live events while the app is on screen: the folder and the drop box follow the server.
    LaunchedEffect(session) {
        val api = container.apiFor(session)
        lifecycle.repeatOnLifecycle(Lifecycle.State.STARTED) {
            liveEvents(api).collect { event ->
                if (event != LiveEvent.DropBox) container.changes.tryEmit(Unit)
                if (event != LiveEvent.Changes) launch { runCatching { container.dropBox.refresh() } }
            }
        }
    }

    // Notices and upload progress show as notifications: ask once, after sign-in, on Android 13+.
    val context = LocalContext.current
    val notifications = rememberLauncherForActivityResult(ActivityResultContracts.RequestPermission()) {}
    LaunchedEffect(Unit) {
        val asked =
            Build.VERSION.SDK_INT < 33 ||
                ContextCompat.checkSelfPermission(context, Manifest.permission.POST_NOTIFICATIONS) == PERMISSION_GRANTED
        if (!asked) notifications.launch(Manifest.permission.POST_NOTIFICATIONS)
    }

    // Push notifications: registered with the user's distributor once signed in, and at each start.
    LaunchedEffect(session) { container.startPush() }

    // A tapped notice opens its page: the drop box, else the app as it is.
    val opened by container.opened.collectAsStateWithLifecycle()
    val ready = backStack != null
    LaunchedEffect(opened, ready) {
        // The graph is set once the NavHost is composed.
        val path = opened?.takeIf { ready } ?: return@LaunchedEffect
        container.opened.value = null
        if (path == "/drop-box") {
            nav.navigate(DropBoxRoute) {
                popUpTo(nav.graph.findStartDestination().id) { saveState = true }
                launchSingleTop = true
                restoreState = true
            }
        }
    }

    Scaffold(
        contentWindowInsets = WindowInsets(0, 0, 0, 0),
        bottomBar = {
            NavigationBar {
                tabs.forEach { tab ->
                    val selected =
                        backStack?.destination?.hierarchy?.any { dest -> tab.types.any { dest.hasRoute(it) } } == true
                    NavigationBarItem(
                        selected = selected,
                        onClick = {
                            nav.navigate(tab.route) {
                                popUpTo(nav.graph.findStartDestination().id) { saveState = true }
                                launchSingleTop = true
                                restoreState = true
                            }
                        },
                        icon = {
                            if (tab.route == DropBoxRoute && waiting > 0) {
                                BadgedBox(
                                    badge = { Badge { Text("$waiting") } },
                                ) { Icon(tab.icon, contentDescription = null) }
                            } else {
                                Icon(tab.icon, contentDescription = null)
                            }
                        },
                        label = { Text(tab.label) },
                    )
                }
            }
        },
    ) { padding ->
        NavHost(nav, startDestination = DrivesRoute, modifier = Modifier.padding(padding)) {
            composable<DrivesRoute> {
                val viewModel =
                    viewModel {
                        DrivesViewModel(container.drives, container.changes, container.files::download)
                    }
                DrivesScreen(viewModel, onOpenFolder = { nav.navigate(FilesRoute(it.toString())) })
            }
            composable<FilesRoute> { entry ->
                val folderId = UUID.fromString(entry.toRoute<FilesRoute>().folderId)
                val viewModel =
                    viewModel(key = "files-$folderId") {
                        FilesViewModel(
                            files = container.files,
                            folderId = folderId,
                            changes = container.changes,
                            uploads = container.uploads.all,
                            enqueue = container::uploadToFolder,
                            encryption = container.e2e,
                        )
                    }
                FilesScreen(
                    viewModel = viewModel,
                    files = container.files,
                    onOpenFolder = { nav.navigate(FilesRoute(it.toString())) },
                    onBack = { nav.popBackStack() },
                )
            }
            composable<DropBoxRoute> {
                val viewModel =
                    viewModel {
                        DropBoxViewModel(container.dropBox, container.uploads.all, container::uploadToDropBox)
                    }
                DropBoxScreen(viewModel, container.files, container.drives::drives)
            }
            composable<SettingsRoute> {
                val viewModel =
                    viewModel {
                        SettingsViewModel(
                            session = session,
                            email = { container.email(session) },
                            notifications = container.push,
                            signOutDevice = { container.signOutDevice(session) },
                            forget = { container.signOutLocally() },
                        )
                    }
                SettingsScreen(viewModel)
            }
        }
    }
}
