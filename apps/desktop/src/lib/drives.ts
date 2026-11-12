import type { Drive, MirrorStopped, Role } from "organizer-sdk/types";
import type { SyncedFolder } from "./desktop";

/** A drive in Settings → Synced folders: all of it synced, or folders in it, or nothing yet. */
export type DriveRow = {
  drive: Drive;
  /** The folder that syncs the whole drive (its root), if one does. */
  whole: SyncedFolder | null;
  /** The folders in it synced on their own. */
  folders: SyncedFolder[];
};

/**
 * The user's drives (as `listDrives` gives them: My drive first, the
 * mirrors last) with their synced folders, the mirrors in a group of their
 * own (docs/M7D_DESIGN.md §5), the drives from other servers in another
 * (docs/M7E_DESIGN.md §6), and the synced folders of no drive listed:
 * a drive left or deleted, or a folder gone from the server. A folder
 * whose drive the engine has not asked yet was synced before drives: My
 * drive's.
 */
export function driveRows(
  drives: Drive[],
  folders: SyncedFolder[],
): { rows: DriveRow[]; mirrors: DriveRow[]; remotes: DriveRow[]; others: SyncedFolder[] } {
  const my = drives.find((d) => d.kind === "my");
  const driveOf = (folder: SyncedFolder) =>
    drives.find((d) => d.root_id === folder.remote_id) ??
    drives.find((d) => d.id === (folder.drive_id ?? my?.id));
  const rows = drives.map((drive) => ({ drive, whole: null, folders: [] }) as DriveRow);
  const others: SyncedFolder[] = [];
  for (const folder of folders) {
    const drive = driveOf(folder);
    const row = rows.find((r) => r.drive.id === drive?.id);
    if (!row) others.push(folder);
    else if (folder.remote_id === row.drive.root_id && !row.whole) row.whole = folder;
    else row.folders.push(folder);
  }
  const grouped = (kind: Drive["kind"]) => rows.filter((r) => r.drive.kind === kind);
  return {
    rows: rows.filter((r) => r.drive.kind !== "mirror" && r.drive.kind !== "remote"),
    mirrors: grouped("mirror"),
    remotes: grouped("remote"),
    others,
  };
}

/** A drive's name as listed: a mirror's says it is one ("Team · mirror"). */
export function driveLabel(drive: Drive): string {
  return drive.kind === "mirror" ? `${drive.name} · mirror` : drive.name;
}

/** Why a mirror stopped copying, in words (the server's `Stopped::reason`). */
export function stoppedReason(stopped: MirrorStopped | null | undefined): string {
  switch (stopped) {
    case "drive_deleted":
      return "The drive was deleted";
    case "access_lost":
      return "You no longer have this drive (or may only upload to it)";
    case "forbidden":
      return "The drive's managers do not allow mirroring it";
    case "shared_drives_off":
      return "Mirroring shared drives is not allowed here";
    default:
      return "It no longer copies";
  }
}

/**
 * Where a mirror stands (docs/M7D_DESIGN.md §5, §9): up to date, copying,
 * waiting for its storage's unlock, failing, or stopped and why; null for
 * a drive that is not a mirror. It is read-only whatever its state.
 */
export function mirrorState(drive: Drive): string | null {
  const of = drive.mirror_of;
  if (drive.kind !== "mirror" || !of) return null;
  switch (of.status) {
    case "up_to_date":
      return "Up to date";
    case "pending":
      return "Changes wait for the next copy";
    case "copying":
      return "Copying…";
    case "waiting_for_unlock":
      return "Waiting for your storage to be unlocked";
    case "failing":
      return "Copying fails; it is tried again in a few minutes";
    case "stopped":
      return `Stopped: ${stoppedReason(of.stopped)}`;
  }
}

/**
 * Whose a drive from another server is and where (docs/M7E_DESIGN.md §5):
 * "From alice@cloud.example", its server named apart when the owner's
 * address does not say it; null for this server's drives.
 */
export function remoteOrigin(drive: Drive): string | null {
  const remote = drive.remote;
  if (drive.kind !== "remote" || !remote) return null;
  const who = remote.owner || remote.shared_by;
  if (!who) return `From ${remote.server}`;
  return who.endsWith(`@${remote.server}`) ? `From ${who}` : `From ${who} on ${remote.server}`;
}

/**
 * Where a drive from another server stands: waiting to be accepted, or its
 * server's last error (unreachable, the token refused); null when it
 * answers, and for this server's drives.
 */
export function remoteState(drive: Drive): string | null {
  const remote = drive.remote;
  if (drive.kind !== "remote" || !remote) return null;
  if (remote.state === "pending") return "Shared with you: accept it to sync it.";
  if (remote.error) return `Its server does not answer now: ${remote.error}`;
  return null;
}

/** Whether a drive can be synced now: it has a folder, its storage answers, and one from another server was accepted. */
export function syncable(drive: Drive): boolean {
  return drive.root_id !== null && drive.available && drive.remote?.state !== "pending";
}

/**
 * Where a drive or folder goes on this computer when no place is typed,
 * as the app chooses it (`local_place` in `src-tauri/src/commands.rs`):
 * `<organizer dir>/<name>`, with " 2", " 3"... when that is a synced
 * folder's place already, or around one (`taken`, the drop box folder's
 * too).
 */
export function defaultPlace(organizerDir: string, name: string, taken: string[]): string {
  const sep = organizerDir.includes("\\") && !organizerDir.includes("/") ? "\\" : "/";
  const within = (path: string, dir: string) => path === dir || path.startsWith(dir + sep);
  const base = name || "Files";
  let place = `${organizerDir}${sep}${base}`;
  for (let n = 2; taken.some((t) => within(place, t) || within(t, place)); n++) {
    place = `${organizerDir}${sep}${base} ${n}`;
  }
  return place;
}

/** What a drive's row says under its name. */
export function driveSummary(row: DriveRow): string {
  if (row.whole) return row.whole.local_path;
  if (row.drive.root_id === null) return "Not available on the server now";
  const n = row.folders.length;
  if (n === 0) return "Not synced";
  return n === 1 ? "1 folder in it synced" : `${n} folders in it synced`;
}

/** A role, in a word or two (docs/M7B_DESIGN.md §1). */
export function roleLabel(role: Role): string {
  const labels: Record<Role, string> = {
    owner: "Owner",
    manager: "Manager",
    editor: "Editor",
    viewer: "Viewer",
    upload_only: "Upload only",
  };
  return labels[role];
}

/** What a role means for syncing, when it holds something back; null when all is synced both ways. */
export function roleNote(role: Role | null | undefined): string | null {
  if (role === "viewer") return "View only: what you change here is not sent.";
  if (role === "upload_only")
    return "Upload only: new files are sent; what you added is not changed, renamed or deleted on the server.";
  return null;
}

/**
 * The role a synced folder shows: its own, when it says more than its
 * drive's (a folder you may do less, or more, with); null otherwise.
 */
export function folderRole(folder: SyncedFolder, drive: Drive | null): Role | null {
  if (!folder.role || folder.role === drive?.my_role) return null;
  return folder.role;
}

/**
 * Where a drive is stored, when its members should know (docs/M7C_DESIGN.md
 * §2, §5): on someone's own storage (it goes if they remove it), and not
 * in the server's backups. Empty for a drive on the server's disk.
 */
export function storageNotes(drive: Drive): string[] {
  const notes: string[] = [];
  // A mirror is always on its user's own storage, a drive from another
  // server on that server's: nothing to warn of.
  if (drive.kind === "mirror" || drive.kind === "remote") return notes;
  if (drive.backend.owner) {
    notes.push(
      `Stored on ${drive.backend.owner}'s own storage. If they remove it, this drive goes.`,
    );
  }
  if (!drive.backend.backed_up) notes.push("Not backed up by the server.");
  return notes;
}

/**
 * A drive's storage now: `locked` (its owner's private credentials wait
 * for them to sign in: the server's `unavailable` says so),
 * `unavailable` (its backend fails), `moving` (to other storage, read-only
 * meanwhile), or null when it syncs.
 */
export function storageKind(drive: Drive): "locked" | "unavailable" | "moving" | null {
  if (!drive.available) return drive.unavailable === "locked" ? "locked" : "unavailable";
  return drive.moving_to ? "moving" : null;
}

/**
 * Why a drive is not synced now (docs/M7C_DESIGN.md §2, §4): its storage
 * unavailable (with the server's reason, "Locked until … signs in" for a
 * locked one), or the drive being moved; null when it syncs.
 */
export function storageState(drive: Drive): string | null {
  if (!drive.available) {
    const reason = drive.unavailable_reason ?? "Its storage cannot be reached";
    return `Not available: ${reason}. Changes made here wait.`;
  }
  if (drive.moving_to) {
    return `Moving to ${drive.moving_to.name}… Read-only meanwhile: changes made here are sent after.`;
  }
  return null;
}

/** Why some synced folders are not synced now, if some are not; null when none. */
export function unavailableNote(folders: SyncedFolder[]): string | null {
  const n = folders.filter((f) => f.unavailable).length;
  if (n === 0) return null;
  const which = n === 1 ? "1 synced folder is" : `${n} synced folders are`;
  return `${which} no longer on the server, or you are no longer a member of ${n === 1 ? "its" : "their"} drive.`;
}
