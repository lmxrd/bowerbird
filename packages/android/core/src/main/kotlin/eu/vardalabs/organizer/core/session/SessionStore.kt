package eu.vardalabs.organizer.core.session

import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.stringPreferencesKey
import eu.vardalabs.organizer.core.e2e.PrivateKeyStore
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import java.util.Base64

/** A signed-in device: the server and its token (an API key of kind `device`). */
data class Session(
    /** The server's base URL, without a trailing slash. */
    val server: String,
    val token: String,
    /** The start of the token, as Settings → Devices on the web shows it. */
    val keyPrefix: String,
    val deviceName: String,
)

/** A sign-in started in the browser: what the redirect is checked against. */
data class PendingSignIn(
    val server: String,
    val verifier: String,
    val deviceName: String,
)

/** Where the session is: still being read from disk, signed out, or signed in. */
sealed interface SessionState {
    data object Loading : SessionState

    data object SignedOut : SessionState

    data class SignedIn(
        val session: Session,
    ) : SessionState
}

/**
 * The session, kept in DataStore with the token encrypted by [TokenCipher]
 * (a key in the Android Keystore). A token that no longer decrypts is
 * treated as signed out. The user's unlocked private key for the encrypted
 * folders is kept the same way, and goes with the session.
 */
class SessionStore(
    private val store: DataStore<Preferences>,
    private val cipher: TokenCipher,
) : PrivateKeyStore {
    private val state = MutableStateFlow<SessionState>(SessionState.Loading)
    private val loading = Mutex()

    /** The session as it changes; [SessionState.Loading] until [load] (or [current]) ran. */
    val sessions: StateFlow<SessionState> = state.asStateFlow()

    /** Read the session from disk, once. */
    suspend fun load(): SessionState =
        loading.withLock {
            state.value.takeIf { it != SessionState.Loading } ?: read().also { state.value = it }
        }

    /** The signed-in session, or null. */
    suspend fun current(): Session? = (load() as? SessionState.SignedIn)?.session

    suspend fun save(session: Session) {
        val encrypted = Base64.getEncoder().encodeToString(cipher.encrypt(session.token.toByteArray()))
        store.edit {
            it[SERVER] = session.server
            it[TOKEN] = encrypted
            it[KEY_PREFIX] = session.keyPrefix
            it[DEVICE_NAME] = session.deviceName
            it.remove(PENDING_SERVER)
            it.remove(PENDING_VERIFIER)
            it.remove(PENDING_DEVICE)
        }
        state.value = SessionState.SignedIn(session)
    }

    /** Forget the session (the server's address is kept, for the next sign-in). */
    suspend fun clear() {
        store.edit {
            it.remove(TOKEN)
            it.remove(KEY_PREFIX)
            it.remove(PRIVATE_KEY)
        }
        state.value = SessionState.SignedOut
    }

    /** The private key, or null when not unlocked here (or it no longer decrypts). */
    override suspend fun loadPrivateKey(): ByteArray? {
        val encrypted = store.data.first()[PRIVATE_KEY] ?: return null
        return runCatching { cipher.decrypt(Base64.getDecoder().decode(encrypted)) }.getOrNull()
    }

    override suspend fun savePrivateKey(key: ByteArray) {
        val encrypted = Base64.getEncoder().encodeToString(cipher.encrypt(key))
        store.edit { it[PRIVATE_KEY] = encrypted }
    }

    override suspend fun clearPrivateKey() {
        store.edit { it.remove(PRIVATE_KEY) }
    }

    /** The last server signed in to, to fill the sign-in screen. */
    suspend fun lastServer(): String? = store.data.first()[SERVER]

    /** Keep a sign-in started in the browser, in case the app is killed meanwhile. */
    suspend fun savePending(pending: PendingSignIn) {
        store.edit {
            it[PENDING_SERVER] = pending.server
            it[PENDING_VERIFIER] = pending.verifier
            it[PENDING_DEVICE] = pending.deviceName
        }
    }

    /** The sign-in started in the browser, removed: a code is good for one try. */
    suspend fun takePending(): PendingSignIn? {
        var pending: PendingSignIn? = null
        store.edit {
            val server = it[PENDING_SERVER]
            val verifier = it[PENDING_VERIFIER]
            if (server != null && verifier != null) {
                pending = PendingSignIn(server, verifier, it[PENDING_DEVICE] ?: "Android")
            }
            it.remove(PENDING_SERVER)
            it.remove(PENDING_VERIFIER)
            it.remove(PENDING_DEVICE)
        }
        return pending
    }

    private suspend fun read(): SessionState {
        val prefs = store.data.first()
        val server = prefs[SERVER]
        val encrypted = prefs[TOKEN]
        if (server == null || encrypted == null) return SessionState.SignedOut
        val token =
            runCatching { String(cipher.decrypt(Base64.getDecoder().decode(encrypted))) }
                .getOrNull() ?: return SessionState.SignedOut
        return SessionState.SignedIn(
            Session(
                server = server,
                token = token,
                keyPrefix = prefs[KEY_PREFIX].orEmpty(),
                deviceName = prefs[DEVICE_NAME].orEmpty(),
            ),
        )
    }

    private companion object {
        val SERVER = stringPreferencesKey("server")
        val TOKEN = stringPreferencesKey("token")
        val KEY_PREFIX = stringPreferencesKey("key_prefix")
        val DEVICE_NAME = stringPreferencesKey("device_name")
        val PRIVATE_KEY = stringPreferencesKey("e2e_private_key")
        val PENDING_SERVER = stringPreferencesKey("pending_server")
        val PENDING_VERIFIER = stringPreferencesKey("pending_verifier")
        val PENDING_DEVICE = stringPreferencesKey("pending_device")
    }
}
