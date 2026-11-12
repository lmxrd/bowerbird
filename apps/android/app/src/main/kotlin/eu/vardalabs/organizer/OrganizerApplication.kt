package eu.vardalabs.organizer

import android.app.Application
import android.app.NotificationChannel
import android.app.NotificationManager
import android.content.Context
import android.net.Uri
import android.provider.Settings
import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.preferencesDataStoreFile
import androidx.work.WorkManager
import eu.vardalabs.organizer.core.api.Api
import eu.vardalabs.organizer.core.api.apiCall
import eu.vardalabs.organizer.core.auth.DeviceSignIn
import eu.vardalabs.organizer.core.auth.HttpDeviceLoginServer
import eu.vardalabs.organizer.core.e2e.ApiEncryptionServer
import eu.vardalabs.organizer.core.e2e.E2eKeys
import eu.vardalabs.organizer.core.session.KeystoreTokenCipher
import eu.vardalabs.organizer.core.session.Session
import eu.vardalabs.organizer.core.session.SessionState
import eu.vardalabs.organizer.core.session.SessionStore
import eu.vardalabs.organizer.data.ApiDrivesRepository
import eu.vardalabs.organizer.data.ApiDropBoxRepository
import eu.vardalabs.organizer.data.ApiFilesRepository
import eu.vardalabs.organizer.data.Folder
import eu.vardalabs.organizer.push.ApiPushServer
import eu.vardalabs.organizer.push.NOTICES_CHANNEL
import eu.vardalabs.organizer.push.PushNotifications
import eu.vardalabs.organizer.push.SharedPrefsPushPrefs
import eu.vardalabs.organizer.push.UnifiedPushDistributor
import eu.vardalabs.organizer.upload.EncryptedUploads
import eu.vardalabs.organizer.upload.PrefsResumeStore
import eu.vardalabs.organizer.upload.Prepared
import eu.vardalabs.organizer.upload.Staging
import eu.vardalabs.organizer.upload.UploadKind
import eu.vardalabs.organizer.upload.UploadRequest
import eu.vardalabs.organizer.upload.UploadWorker
import eu.vardalabs.organizer.upload.Uploads
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.channels.BufferOverflow
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.SharingStarted
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.map
import kotlinx.coroutines.flow.stateIn
import kotlinx.coroutines.launch
import kotlinx.coroutines.withContext
import okhttp3.OkHttpClient
import java.io.File
import java.io.IOException
import java.util.concurrent.TimeUnit

class OrganizerApplication : Application() {
    lateinit var container: AppContainer
        private set

    override fun onCreate() {
        super.onCreate()
        container = AppContainer(this)
        getSystemService(NotificationManager::class.java).createNotificationChannels(
            listOf(
                NotificationChannel(
                    UploadWorker.CHANNEL,
                    getString(R.string.uploads),
                    NotificationManager.IMPORTANCE_LOW,
                ),
                NotificationChannel(
                    NOTICES_CHANNEL,
                    getString(R.string.notifications),
                    NotificationManager.IMPORTANCE_DEFAULT,
                ),
            ),
        )
        container.uploads.prune()
    }
}

/** What the app's parts share; made once, by the application. */
class AppContainer(
    context: Context,
) {
    val scope = CoroutineScope(SupervisorJob() + Dispatchers.Default)

    private val http: OkHttpClient =
        OkHttpClient
            .Builder()
            .connectTimeout(20, TimeUnit.SECONDS)
            .readTimeout(60, TimeUnit.SECONDS)
            .writeTimeout(60, TimeUnit.SECONDS)
            .build()

    val sessions =
        SessionStore(
            PreferenceDataStoreFactory.create(produceFile = { context.preferencesDataStoreFile("session") }),
            KeystoreTokenCipher(),
        )

    val signIn = DeviceSignIn(sessions, HttpDeviceLoginServer(http))

    /** The API of the signed-in session, or null. */
    val api: StateFlow<Api?> =
        sessions.sessions
            .map { (it as? SessionState.SignedIn)?.let { signedIn -> apiFor(signedIn.session) } }
            .stateIn(scope, SharingStarted.Eagerly, null)

    /** The keys of the encrypted folders: unlocked once per device, forgotten on sign out. */
    val e2e = E2eKeys({ currentApi()?.let(::ApiEncryptionServer) }, sessions, scope)

    val drives = ApiDrivesRepository(::currentApi)
    val files = ApiFilesRepository(::currentApi, context.cacheDir, drives, e2e::keyring)
    val dropBox = ApiDropBoxRepository(::currentApi)
    val staging = Staging(context)
    val resumeStore = PrefsResumeStore(context)
    val uploads = Uploads(WorkManager.getInstance(context))
    val encryptedUploads =
        EncryptedUploads(e2e::keyring, { id -> files.children(id).map { it.name } }, staging::seal, files::sealingId)

    /** This device's name, as Settings → About shows it. */
    val deviceName: String =
        Settings.Global.getString(context.contentResolver, Settings.Global.DEVICE_NAME)
            ?: "${android.os.Build.MANUFACTURER} ${android.os.Build.MODEL}"

    /** Push notifications through the user's UnifiedPush distributor. */
    val push =
        PushNotifications(
            server = { currentApi()?.let(::ApiPushServer) },
            distributor = UnifiedPushDistributor(context),
            prefs = SharedPrefsPushPrefs(context),
            deviceName = android.os.Build.MODEL,
        )

    /** The device login's redirect back from the browser, until the sign-in screen takes it. */
    val redirects = MutableStateFlow<String?>(null)

    /** The page a tapped notice opens (e.g. `/drop-box`), until the main screen takes it. */
    val opened = MutableStateFlow<String?>(null)

    /** The live events said the files changed: the folder on screen reloads. */
    val changes = MutableSharedFlow<Unit>(extraBufferCapacity = 1, onBufferOverflow = BufferOverflow.DROP_OLDEST)

    private val cacheDir = context.cacheDir

    init {
        scope.launch { sessions.load() }
    }

    /**
     * Picked files into a folder: their access kept, then uploaded by the
     * worker; into an encrypted folder, encrypted here first.
     */
    suspend fun uploadToFolder(
        uris: List<Uri>,
        folder: Folder,
    ) {
        if (folder.e2eRoot == null) {
            enqueue(uris) {
                UploadRequest(
                    UploadKind.FOLDER,
                    it.name,
                    it.source,
                    it.size,
                    it.staged,
                    folderId = folder.id.toString(),
                )
            }
        } else {
            val prepared = withContext(Dispatchers.IO) { uris.map { staging.prepare(it) } }
            encryptedUploads.requests(folder, prepared).forEach { uploads.enqueue(it) }
        }
    }

    /** Picked files into the drop box, with the user's settings. */
    suspend fun uploadToDropBox(uris: List<Uri>) =
        enqueue(uris) { UploadRequest(UploadKind.DROP_BOX, it.name, it.source, it.size, it.staged) }

    private suspend fun enqueue(
        uris: List<Uri>,
        request: (Prepared) -> UploadRequest,
    ) = withContext(Dispatchers.IO) {
        uris.forEach { uploads.enqueue(request(staging.prepare(it))) }
    }

    /** The API of the signed-in session, once the session is read from disk; null when signed out. */
    suspend fun currentApi(): Api? {
        val session = sessions.current() ?: return null
        return api.value?.takeIf { it.session == session } ?: apiFor(session)
    }

    fun apiFor(session: Session) = Api(session, http) { scope.launch { signOutLocally() } }

    /** Register with the distributor, after sign-in and at each start (the connector's database is on disk). */
    suspend fun startPush() =
        withContext(Dispatchers.IO) {
            try {
                push.start()
            } catch (_: IOException) {
                // Offline: the next start registers.
            }
        }

    /** Revoke the session's token on the server (`DELETE /api/auth/device`). */
    suspend fun signOutDevice(session: Session) {
        val api = apiFor(session)
        apiCall { api.auth.signOutDevice() }
    }

    /** The email of the user the session's token belongs to (`GET /api/auth/session`). */
    suspend fun email(session: Session): String? {
        val api = apiFor(session)
        return apiCall { api.auth.getSession() }.user?.email
    }

    /** Forget the session here: what waits to upload is dropped, the cache emptied. */
    suspend fun signOutLocally() {
        withContext(Dispatchers.IO) { push.forget() }
        e2e.lock()
        sessions.clear()
        uploads.cancelAll()
        dropBox.clear()
        withContext(Dispatchers.IO) { File(cacheDir, "open").deleteRecursively() }
    }
}
