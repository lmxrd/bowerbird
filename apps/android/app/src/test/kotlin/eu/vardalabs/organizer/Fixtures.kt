package eu.vardalabs.organizer

import eu.vardalabs.organizer.api.models.BackendKind
import eu.vardalabs.organizer.api.models.BackendRef
import eu.vardalabs.organizer.api.models.Drive
import eu.vardalabs.organizer.api.models.DriveKind
import eu.vardalabs.organizer.api.models.DropBoxSettings
import eu.vardalabs.organizer.api.models.DroppedFolders
import eu.vardalabs.organizer.api.models.FolderFreedom
import eu.vardalabs.organizer.api.models.IncomingShareState
import eu.vardalabs.organizer.api.models.ItemKind
import eu.vardalabs.organizer.api.models.ItemView
import eu.vardalabs.organizer.api.models.Kind
import eu.vardalabs.organizer.api.models.Links
import eu.vardalabs.organizer.api.models.MirrorOf
import eu.vardalabs.organizer.api.models.MirrorSchedule
import eu.vardalabs.organizer.api.models.MirrorStatus
import eu.vardalabs.organizer.api.models.MirrorStopped
import eu.vardalabs.organizer.api.models.Node
import eu.vardalabs.organizer.api.models.PendingMode
import eu.vardalabs.organizer.api.models.PlacementView
import eu.vardalabs.organizer.api.models.ReadContents
import eu.vardalabs.organizer.api.models.RemoteDrive
import eu.vardalabs.organizer.api.models.Role
import eu.vardalabs.organizer.api.models.Status
import eu.vardalabs.organizer.api.models.Unavailable
import eu.vardalabs.organizer.core.e2e.Unlocker
import eu.vardalabs.organizer.core.e2e.WrongPassphraseException
import eu.vardalabs.organizer.data.Folder
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.rules.TestWatcher
import org.junit.runner.Description
import java.util.UUID

/** The keys of the encrypted folders, unlocked by [PASSPHRASE]. */
class FakeUnlocker(
    unlocked: Boolean = false,
) : Unlocker {
    override val unlocked = MutableStateFlow(unlocked)

    override suspend fun isUnlocked() = unlocked.value

    override suspend fun unlock(passphrase: String) {
        if (passphrase != PASSPHRASE) throw WrongPassphraseException()
        unlocked.value = true
    }

    companion object {
        const val PASSPHRASE = "correct horse"
    }
}

/** viewModelScope runs on Dispatchers.Main: a test dispatcher stands in for it. */
@OptIn(ExperimentalCoroutinesApi::class)
class MainDispatcherRule : TestWatcher() {
    override fun starting(description: Description) = Dispatchers.setMain(UnconfinedTestDispatcher())

    override fun finished(description: Description) = Dispatchers.resetMain()
}

/** My drive's id in the tests. */
const val MY_DRIVE = "6f1c2a3e-0000-4000-8000-000000000001"

/** My drive's root: the user owns it. */
val ROOT = Folder(UUID.randomUUID(), "My drive", "/", null, driveName = "My drive", role = Role.OWNER)

fun node(
    name: String,
    kind: Kind = Kind.FILE,
    parent: UUID = ROOT.id,
    version: Long = 1,
    e2eRoot: UUID? = null,
    id: UUID = UUID.randomUUID(),
    driveId: String = MY_DRIVE,
    /** The user's role on it (`my_role`): theirs in My drive. */
    role: Role? = Role.OWNER,
    /** In a mirror: the node it copies. */
    sourceId: UUID? = null,
    /** Shared from another server: that server. */
    remote: String? = null,
) = Node(
    remote = remote,
    createdAt = "2026-11-07T10:00:00Z",
    driveId = driveId,
    e2eRoot = e2eRoot,
    sourceId = sourceId,
    id = id,
    kind = kind,
    mime = if (kind == Kind.FILE) "application/pdf" else null,
    myRole = role,
    name = name,
    parentId = parent,
    portable = true,
    propertySize = 1234,
    updatedAt = "2026-11-07T10:00:00Z",
    version = version,
)

fun item(
    name: String,
    status: Status,
    placed: Boolean = false,
) = ItemView(
    createdAt = "t",
    id = UUID.randomUUID(),
    kind = ItemKind.FILE,
    name = name,
    readContents = true,
    status = status,
    updatedAt = "t",
    placement =
        if (placed) {
            PlacementView(
                appliedAt = "t",
                id = UUID.randomUUID(),
                nodeId = UUID.randomUUID(),
            )
        } else {
            null
        },
)

fun settings(
    readContents: ReadContents = ReadContents.PER_ITEM,
    links: Links = Links.BOOKMARK,
) = DropBoxSettings(
    folderFreedom = FolderFreedom.BALANCED,
    folders = DroppedFolders.WHOLE,
    links = links,
    pendingMode = PendingMode.STAY,
    readContents = readContents,
)

/** A drive as `GET /api/drives` answers it: a shared drive unless [kind] says otherwise. */
fun drive(
    name: String,
    rootId: UUID? = UUID.randomUUID(),
    kind: DriveKind = DriveKind.SHARED,
    members: Long = 3,
    used: Long = 2048,
    id: String = UUID.randomUUID().toString(),
    /** The user's role on the drive: owner of My drive, editor (a member) of a shared drive. */
    role: Role? = if (kind == DriveKind.MY) Role.OWNER else Role.EDITOR,
    /** Where it is stored: the server's disk unless said. */
    backend: BackendRef = DISK,
    /** Why its storage is unavailable; null: it is available. */
    unavailable: String? = null,
    /** Its storage is unavailable because it is locked (else failing). */
    locked: Boolean = false,
    /** Where it is being moved, if it is. */
    movingTo: BackendRef? = null,
    /** What a mirror copies, and how it stands (with [kind] mirror). */
    mirrorOf: MirrorOf? = null,
    /** Whose and where a drive of another server is (with [kind] remote). */
    remote: RemoteDrive? = null,
) = Drive(
    remote = remote,
    backend = backend,
    available = unavailable == null,
    unavailable =
        when {
            unavailable == null -> null
            locked -> Unavailable.LOCKED
            else -> Unavailable.FAILING
        },
    unavailableReason = unavailable,
    movingTo = movingTo,
    mirrorOf = mirrorOf,
    id = id,
    kind = kind,
    limit = null,
    manages = role == Role.MANAGER || role == Role.OWNER,
    managers = if (kind == DriveKind.MY) emptyList() else listOf("kari@example.com"),
    members = members,
    myRole = role,
    name = name,
    rootId = rootId,
    teamId = null,
    teamName = null,
    used = used,
)

/** A mirror of [of] (a drive named so), viewed, on the user's own storage, standing at [status]. */
fun mirror(
    of: String,
    status: MirrorStatus = MirrorStatus.UP_TO_DATE,
    stopped: MirrorStopped? = null,
    rootId: UUID? = UUID.randomUUID(),
) = drive(
    of,
    rootId = rootId,
    kind = DriveKind.MIRROR,
    members = 1,
    role = Role.VIEWER,
    backend = bucket("Anna's", owner = "anna@example.com"),
    mirrorOf =
        MirrorOf(
            driveId = UUID.randomUUID().toString(),
            driveName = of,
            restoresIntoDrive = true,
            schedule = MirrorSchedule.CONTINUOUS,
            status = status,
            stopped = stopped,
            stoppedAt = null,
        ),
)

/** Where a share from another server comes from: alice's on cloud.example, standing at [state]. */
fun fromAlice(
    state: IncomingShareState = IncomingShareState.ACCEPTED,
    error: String? = null,
) = RemoteDrive(
    error = error,
    owner = "alice@cloud.example",
    ownerName = "Alice",
    server = "cloud.example",
    sharedBy = "alice@cloud.example",
    state = state,
)

/** A drive shared from another server ([remote]: alice's), with the role it was given there. */
fun remoteDrive(
    name: String,
    remote: RemoteDrive = fromAlice(),
    role: Role = Role.EDITOR,
    rootId: UUID? = UUID.randomUUID(),
) = drive(name, rootId = rootId, kind = DriveKind.REMOTE, members = 1, role = role, remote = remote)

/** The server's disk, where drives are unless said (the nil UUID in the API). */
val DISK =
    BackendRef(
        backedUp = true,
        id = UUID(0, 0),
        kind = BackendKind.DISK,
        name = "The server's disk",
        owner = null,
    )

/** An S3 storage: someone's own ([owner], not backed up by the server), or a global one. */
fun bucket(
    name: String,
    owner: String? = null,
) = BackendRef(
    backedUp = false,
    id = UUID.randomUUID(),
    kind = BackendKind.S3,
    name = name,
    owner = owner,
)

/** My drive, whose root is [ROOT]. */
val MY = drive("My drive", rootId = ROOT.id, kind = DriveKind.MY, members = 1, id = MY_DRIVE)
