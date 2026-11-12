import type {
  Drive,
  Kind,
  Mirror,
  MirrorProgress,
  MirrorSchedule,
  MirrorStatus,
  MirrorStopped,
  Node,
  Role,
  TrashItem,
} from "organizer-sdk";
import { errorCode } from "@/lib/api";
import { formatDate, formatDay } from "@/lib/format";
import { can } from "@/lib/roles";

/**
 * Mirrors (docs/M7D_DESIGN.md): read-only copies of a drive in the user's
 * own storage. Their words, and what the pages offer, from what the server
 * says of each (`Mirror`, `Drive.mirror_of`).
 */

/** The schedules, as the "When" select offers them (the first the default). */
export const SCHEDULES: readonly { value: MirrorSchedule; label: string }[] = [
  { value: "continuous", label: "Continuously" },
  { value: "hourly", label: "Every hour" },
  { value: "daily", label: "Every day" },
  { value: "manual", label: "Only when I ask" },
];

/** A schedule for people: "Continuously", "Every hour". */
export function scheduleLabel(schedule: MirrorSchedule): string {
  return SCHEDULES.find((s) => s.value === schedule)?.label ?? schedule;
}

/** A mirror's status for its badge. */
export const STATUS_LABELS = {
  up_to_date: "Up to date",
  pending: "Pending",
  copying: "Copying",
  waiting_for_unlock: "Waiting for unlock",
  failing: "Failing",
  stopped: "Stopped",
} as const satisfies Record<MirrorStatus, string>;

/** The badge's variant: what needs the user stands out. */
export function statusVariant(status: MirrorStatus): "secondary" | "outline" | "destructive" {
  if (status === "failing" || status === "stopped") return "destructive";
  if (status === "up_to_date") return "secondary";
  return "outline";
}

/** Why a mirror stopped, to follow "Mirroring stopped: ". */
const STOPPED_WHY = {
  drive_deleted: "the drive was deleted",
  access_lost: "you no longer have this drive, or may only upload to it",
  forbidden: "the drive's managers don't allow mirroring it",
  shared_drives_off: "mirroring shared drives isn't allowed here",
} as const satisfies Record<MirrorStopped, string>;

/** "Stopped: the drive was deleted", for a row's badge and detail. */
export function stoppedText(stopped: MirrorStopped): string {
  return `Stopped: ${STOPPED_WHY[stopped]}`;
}

/** When a mirror stopped, to follow its reason: "today", "on 2 Nov"; none when not known. */
export function stoppedWhen(stoppedAt: string | null | undefined, now: Date = new Date()): string {
  if (!stoppedAt) return "";
  return new Date(stoppedAt).toDateString() === now.toDateString()
    ? " today"
    : ` on ${formatDay(stoppedAt, now)}`;
}

/**
 * The banner of a mirror's page when it does not copy now: stopped (why,
 * and when; a deleted drive's mirror stays as it is, the others go on when
 * the reason goes), waiting for an unlock, or failing (the server's words).
 */
export function statusBanner(
  mirror: Pick<Mirror, "status" | "stopped" | "stopped_at" | "error">,
  now: Date = new Date(),
): string | undefined {
  if (mirror.status === "stopped" && mirror.stopped) {
    const keep = "This copy stays until you remove it.";
    const why = `Mirroring stopped: ${STOPPED_WHY[mirror.stopped]}${stoppedWhen(mirror.stopped_at, now)}.`;
    return mirror.stopped === "drive_deleted"
      ? `${why} ${keep}`
      : `${why} It goes on once that changes. ${keep}`;
  }
  if (mirror.status === "waiting_for_unlock") {
    return "Waiting for unlock: the storage's credentials are locked. It copies again once you sign in and unlock them.";
  }
  if (mirror.status === "failing") {
    return `The last copy failed: ${(mirror.error ?? "an error").replace(/\.$/, "")}. It is tried again in a few minutes.`;
  }
  return undefined;
}

/** A count for people: "1,204". */
function count(n: number): string {
  return n.toLocaleString("en-US");
}

/** "copying 1,204 of 3,880 files". */
export function progressText(progress: MirrorProgress): string {
  const unit = progress.files_total === 1 ? "file" : "files";
  return `copying ${count(progress.files_done)} of ${count(progress.files_total)} ${unit}`;
}

/**
 * Where a mirror's copy is: "copying 1,204 of 3,880 files" while it
 * copies, else "last copied <when>", or "not copied yet".
 */
export function lastCopyText(
  mirror: Pick<Mirror, "status" | "progress" | "last_copy_at">,
  now: Date = new Date(),
): string {
  if (mirror.status === "copying" && mirror.progress) return progressText(mirror.progress);
  if (mirror.last_copy_at) return `last copied ${formatDate(mirror.last_copy_at, now)}`;
  return mirror.status === "copying" ? "copying" : "not copied yet";
}

/** A mirror's page title: "Team · mirror". */
export function mirrorTitle(name: string): string {
  return `${name} · mirror`;
}

/**
 * What a restore from a mirror's trash or versions is: copied back into
 * the drive mirrored when the user may write there (and it is still
 * theirs), else downloaded. A folder cannot be downloaded: none then.
 */
export function restoreAction(
  mirror: Pick<Mirror, "restores_into_drive" | "stopped">,
  kind: Kind,
): "restore" | "download" | undefined {
  const gone = mirror.stopped === "drive_deleted" || mirror.stopped === "access_lost";
  if (mirror.restores_into_drive && !gone) return "restore";
  return kind === "file" ? "download" : undefined;
}

/** The refusals of the mirrors' endpoints, by their `code`. */
const REFUSALS: Record<string, string> = {
  access_lost: "You may not mirror this drive: you don't have it, or may only upload to it.",
  shared_drives_off: "Mirroring shared drives isn't allowed here. Your administrator can allow it.",
  mirroring_forbidden: "The drive's managers don't allow mirroring it.",
  drive_deleted: "The drive mirrored was deleted.",
  mirror_stopped: "This mirror stopped: it copies again once the reason goes.",
  drive_read_only: "You may not add to the drive mirrored: download it instead.",
  copy_running: "The mirror is copying now: try again once it is done.",
  name_taken: "A shared drive has that name already: choose another.",
};

/** A refusal's words by its `code`; none for an error without one we know. */
export function refusalText(error: unknown): string | undefined {
  const code = errorCode(error);
  return code ? REFUSALS[code] : undefined;
}

/** Whether an eject was refused for its name (`name_taken`): the dialog asks for another. */
export function nameTaken(error: unknown): boolean {
  return errorCode(error) === "name_taken";
}

/**
 * Whether a drive's page offers "Mirror…": the user has storage of their
 * own; My drive to its user; a shared drive to a viewer and above, unless
 * its managers forbid it (`mirrors.allowed` false). Whether shared drives
 * may be mirrored at all only the server knows: it refuses with a code.
 */
export function mayMirror(
  drive: Pick<Drive, "kind" | "my_role">,
  ownBackends: number,
  allowed: boolean | undefined,
): boolean {
  if (ownBackends === 0) return false;
  if (drive.kind === "my") return true;
  if (drive.kind !== "shared") return false;
  return can(drive.my_role as Role | null, "read") && allowed !== false;
}

/**
 * A node as its encrypted folder knows it, in a mirror (docs/M7D_DESIGN.md
 * §9) or shared from another server (docs/E2E_SPEC.md §12): the keys and
 * names are sealed with the ids where it comes from, so the node's id is
 * its `source_id`, its parent the parent's `source_id` (a name is sealed
 * with its parent's, even for a node that has no source yet: a file put
 * there a moment ago), its root the encrypted folder's top's `source_id`.
 * A node of any other drive is as it is.
 */
export function sealedAs<T extends Pick<Node, "id" | "parent_id" | "e2e_root" | "source_id">>(
  node: T,
  parentSource: string | null | undefined,
  rootSource: string | null | undefined,
): T {
  if (node.e2e_root == null || (!node.source_id && !parentSource)) return node;
  return {
    ...node,
    id: node.source_id ?? node.id,
    parent_id: parentSource ?? node.parent_id,
    e2e_root: rootSource ?? node.e2e_root,
  };
}

/**
 * An item of a trash as its encrypted folder knows it, for its name: where
 * it was (`original_parent_id`), and in a mirror (or a drive ejected from
 * one) the drive's ids it kept (`sealedAs`).
 */
export function trashSealedAs(
  item: Pick<
    TrashItem,
    | "id"
    | "name"
    | "original_parent_id"
    | "e2e_root"
    | "source_id"
    | "original_parent_source_id"
    | "e2e_root_source_id"
  >,
): Pick<Node, "id" | "name" | "parent_id" | "e2e_root" | "source_id"> {
  return sealedAs(
    {
      id: item.id,
      name: item.name,
      parent_id: item.original_parent_id,
      e2e_root: item.e2e_root,
      source_id: item.source_id,
    },
    item.original_parent_source_id,
    item.e2e_root_source_id,
  );
}

/**
 * The name an ejected mirror's drive gets: its own unless the user knows a
 * drive of that name (drive names are unique among the shared drives),
 * then the first of "<name> (copy)", "<name> (copy 2)" that is free.
 * `needed`: the user should see (and may change) it.
 */
export function ejectName(name: string, taken: string[]): { name: string; needed: boolean } {
  const used = new Set(taken.map((t) => t.toLowerCase()));
  if (!used.has(name.toLowerCase())) return { name, needed: false };
  for (let n = 1; ; n++) {
    const next = n === 1 ? `${name} (copy)` : `${name} (copy ${n})`;
    if (!used.has(next.toLowerCase())) return { name: next, needed: true };
  }
}
