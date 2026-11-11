package eu.vardalabs.organizer.core.e2e

import eu.vardalabs.organizer.api.models.EncryptionKeys
import eu.vardalabs.organizer.api.models.EncryptionTask
import eu.vardalabs.organizer.api.models.FolderKeys
import eu.vardalabs.organizer.api.models.MemberKey
import eu.vardalabs.organizer.api.models.NewFolderKeys
import eu.vardalabs.organizer.core.api.Api
import eu.vardalabs.organizer.core.api.ApiException
import eu.vardalabs.organizer.core.api.apiCall
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow
import kotlinx.coroutines.launch
import kotlinx.coroutines.sync.Mutex
import kotlinx.coroutines.sync.withLock
import kotlinx.coroutines.withContext
import java.io.IOException
import java.util.UUID

/** The encrypted folder cannot be opened before the keys are unlocked on this device. */
class LockedException : E2eException("Unlock to see what's inside")

/** The user has not set up encryption (the web does that). */
class NotSetUpException : E2eException("Set up encryption on the web first")

/** The passphrase does not open the private key. */
class WrongPassphraseException : E2eException("That passphrase does not unlock your keys")

/**
 * Shared from another server (docs/E2E_SPEC.md §12), whose server does not
 * answer for the keys (502 `remote_unreachable`).
 */
class OwnerUnreachableException : E2eException("The owner's server is not reachable. Try again later.")

/** The server's part of the encrypted folders (docs/E2E_SPEC.md §7). */
interface EncryptionServer {
    /** `GET /api/encryption`. */
    suspend fun keys(): EncryptionKeys

    /** `GET /api/encryption/folders/{id}`: any node inside answers for its encrypted folder. */
    suspend fun folderKeys(id: UUID): FolderKeys

    /** `POST /api/encryption/folders/{id}/keys`; a 409 when another device was first. */
    suspend fun addFolderKeys(
        id: UUID,
        keys: List<MemberKey>,
    ): FolderKeys

    /** `GET /api/encryption/pending`. */
    suspend fun pending(): List<EncryptionTask>
}

/** [EncryptionServer] with the generated client. */
class ApiEncryptionServer(
    private val api: Api,
) : EncryptionServer {
    override suspend fun keys() = apiCall { api.encryption.getEncryption() }

    override suspend fun folderKeys(id: UUID) = apiCall { api.encryption.getFolderKeys(id) }

    override suspend fun addFolderKeys(
        id: UUID,
        keys: List<MemberKey>,
    ) = apiCall { api.encryption.addFolderKeys(id, NewFolderKeys(keys)) }

    override suspend fun pending() = apiCall { api.encryption.listEncryptionTasks() }
}

/** Where the unlocked private key is kept between starts (encrypted, as the token). */
interface PrivateKeyStore {
    suspend fun loadPrivateKey(): ByteArray?

    suspend fun savePrivateKey(key: ByteArray)

    suspend fun clearPrivateKey()
}

/** Whether this device holds the user's keys, and how it gets them. */
interface Unlocker {
    /** Changes when the keys are unlocked or forgotten. */
    val unlocked: StateFlow<Boolean>

    /** Unlocked, once the kept key was read from disk. */
    suspend fun isUnlocked(): Boolean

    /** Unlock with the encryption passphrase; a [WrongPassphraseException] when it does not open the key. */
    suspend fun unlock(passphrase: String)
}

/**
 * This device's keys: the user's private key, unlocked once with the
 * passphrase and then kept in [store] until sign out, and the folder keys
 * unwrapped with it, per encrypted folder, in memory only.
 *
 * It does the device's duties too (docs/E2E_SPEC.md §4): after unlocking,
 * and when an encrypted folder's keys are first read, it wraps the
 * generations members are waiting for and makes the next generation when
 * one is needed.
 */
class E2eKeys(
    private val server: suspend () -> EncryptionServer?,
    private val store: PrivateKeyStore,
    private val scope: CoroutineScope,
) : Unlocker {
    private val lock = Mutex()
    private var private: ByteArray? = null
    private var loaded = false
    private var pendingChecked = false
    private val keyrings = mutableMapOf<UUID, Keyring>()
    private val state = MutableStateFlow(false)

    override val unlocked: StateFlow<Boolean> = state.asStateFlow()

    private suspend fun privateKey(): ByteArray? =
        lock.withLock {
            if (!loaded) {
                private = store.loadPrivateKey()?.takeIf { it.size == 32 }
                loaded = true
                state.value = private != null
            }
            private
        }

    override suspend fun isUnlocked(): Boolean = privateKey() != null

    private suspend fun server(): EncryptionServer = server.invoke() ?: throw ApiException(401, "Signed out")

    override suspend fun unlock(passphrase: String) {
        val server = server()
        val keys = server.keys()
        val wrap = keys.passphrase
        val public = keys.publicKey
        if (!keys.setUp || wrap == null || public == null) throw NotSetUpException()
        // PBKDF2's 600,000 rounds take a while: not on the main thread.
        val private =
            withContext(Dispatchers.Default) {
                try {
                    E2e.unwrapWithPassphrase(E2e.unb64(wrap.wrapped), passphrase, E2e.unb64(wrap.salt), wrap.iterations)
                } catch (_: DecryptException) {
                    throw WrongPassphraseException()
                }
            }
        // The server's two copies must agree, or what is wrapped for the public key would not open.
        if (!E2e.publicKey(private).contentEquals(E2e.unb64(public))) {
            throw E2eException("Your keys on the server do not match: set up encryption again on the web")
        }
        store.savePrivateKey(private)
        lock.withLock {
            this.private = private
            loaded = true
            pendingChecked = true
            keyrings.clear()
        }
        state.value = true
        scope.launch { quietly { seeToPending() } }
    }

    /** Forget the keys here (sign out). */
    suspend fun lock() {
        lock.withLock {
            private = null
            loaded = true
            pendingChecked = false
            keyrings.clear()
        }
        store.clearPrivateKey()
        state.value = false
    }

    /**
     * The keys of the encrypted folder [root] (its top's id): the ones in
     * memory, or read from the server when [fresh] (before a write, so that
     * it uses the newest generation) or not read yet.
     */
    suspend fun keyring(
        root: UUID,
        fresh: Boolean = false,
    ): Keyring {
        if (!fresh) lock.withLock { keyrings[root] }?.let { return it }
        val private = privateKey() ?: throw LockedException()
        val server = server()
        val asked =
            try {
                server.folderKeys(root)
            } catch (e: ApiException) {
                if (e.remoteUnreachable) throw OwnerUnreachableException()
                throw e
            }
        val keys = seeTo(server, asked, private)
        val ring = unwrap(keys, private)
        val checkPending =
            lock.withLock {
                keyrings[root] = ring
                (!pendingChecked).also { pendingChecked = true }
            }
        if (checkPending) scope.launch { quietly { seeToPending() } }
        return ring
    }

    /** The folders with waiting members or in need of a new key, seen to (`GET /api/encryption/pending`). */
    suspend fun seeToPending() {
        val private = privateKey() ?: return
        val server = server()
        for (task in server.pending()) {
            quietly {
                val keys = seeTo(server, server.folderKeys(task.folderId), private)
                val ring = unwrap(keys, private)
                lock.withLock { keyrings[ring.folderId] = ring }
            }
        }
    }

    /**
     * One folder's duties: the next generation when it needs one (for every
     * member with a public key, this user included), then the generations
     * members lack. Answers the folder's keys after them. A duty that fails
     * is left to the next time; reading goes on with what there is.
     */
    private suspend fun seeTo(
        server: EncryptionServer,
        keys: FolderKeys,
        private: ByteArray,
    ): FolderKeys {
        var current = keys
        if (current.needsNewKey) {
            current = quietly(current) { post(server, current.folderId, newGeneration(current, E2e.random(32))) }
        }
        val missing = missingKeys(current, unwrap(current, private))
        if (missing.isNotEmpty()) current = quietly(current) { post(server, current.folderId, missing) }
        return current
    }

    /** Post keys; when another device was first (409), the folder's keys as they are now. */
    private suspend fun post(
        server: EncryptionServer,
        folder: UUID,
        keys: List<MemberKey>,
    ): FolderKeys =
        try {
            server.addFolderKeys(folder, keys)
        } catch (e: ApiException) {
            if (e.status != 409) throw e
            server.folderKeys(folder)
        }

    companion object {
        /**
         * The next generation, [key], wrapped for every member with a public
         * key (the server refuses one that leaves any out). A member on
         * another server (`remote`) is wrapped for as any, by their
         * `user_id`, which is the share's id: never looked up as a user here.
         */
        fun newGeneration(
            keys: FolderKeys,
            key: ByteArray,
        ): List<MemberKey> {
            val generation = keys.generation.toInt() + 1
            return keys.members.mapNotNull { member ->
                member.publicKey?.let {
                    MemberKey(generation.toLong(), member.userId, wrapFor(key, keys.folderId, generation, it))
                }
            }
        }

        /** The generations [ring] holds, for each member with a public key who lacks them (on another server too). */
        fun missingKeys(
            keys: FolderKeys,
            ring: Keyring,
        ): List<MemberKey> =
            keys.members.flatMap { member ->
                val public = member.publicKey ?: return@flatMap emptyList()
                member.missing.mapNotNull { generation ->
                    ring.key(generation.toInt())?.let {
                        MemberKey(generation, member.userId, wrapFor(it, keys.folderId, generation.toInt(), public))
                    }
                }
            }

        private fun wrapFor(
            key: ByteArray,
            folder: UUID,
            generation: Int,
            public: String,
        ) = E2e.b64(E2e.wrapFolderKey(key, folder, generation, E2e.unb64(public)))
    }

    /** Every generation wrapped for this user that opens; one that does not is left out. */
    private fun unwrap(
        keys: FolderKeys,
        private: ByteArray,
    ): Keyring {
        val unwrapped = mutableMapOf<Int, ByteArray>()
        for (wrapped in keys.propertyKeys) {
            val generation = wrapped.generation.toInt()
            try {
                unwrapped[generation] =
                    E2e.unwrapFolderKey(E2e.unb64(wrapped.wrapped), keys.folderId, generation, private)
            } catch (_: E2eException) {
                // Not ours to open: a name or file of that generation shows as encrypted.
            }
        }
        return Keyring(keys.folderId, keys.generation.toInt(), unwrapped)
    }

    private suspend fun <T> quietly(
        fallback: T,
        block: suspend () -> T,
    ): T =
        try {
            block()
        } catch (_: IOException) {
            fallback
        }

    private suspend fun quietly(block: suspend () -> Unit) = quietly(Unit, block)
}
