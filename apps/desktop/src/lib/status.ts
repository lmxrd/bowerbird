import type {
  ActivityKind,
  Encryption,
  FolderMode,
  Held,
  NotSynced,
  Status,
  Waiting,
} from "./desktop";

/** The status line, as the tray says it: Up to date, Syncing N, Paused, Offline, or the error. */
export function headline(status: Status, paused: boolean): string {
  if (paused || status.phase === "paused") return "Paused";
  switch (status.phase) {
    case "offline":
      return "Offline";
    case "error":
      return status.error ? `Error: ${status.error}` : "Error";
    case "syncing":
      return status.pending > 0 ? `Syncing ${status.pending}` : "Up to date";
    default:
      return "Up to date";
  }
}

/** How far a round of changes is, from 0 to 100; null when none is under way. */
export function progress(status: Status): number | null {
  if (status.phase !== "syncing" || status.pending === 0) return null;
  return Math.round((status.done / (status.done + status.pending)) * 100);
}

/** What happened, in a word, for the list of recent activity. */
export function activityLabel(kind: ActivityKind): string {
  const labels: Record<ActivityKind, string> = {
    upload: "Uploaded",
    download: "Downloaded",
    move: "Moved",
    delete: "Deleted",
    conflict: "Conflict",
    drop_box: "Sent to the drop box",
    restore: "Brought back: not yours to delete",
  };
  return labels[kind];
}

/** Why something here is not synced, in a few words. */
export function heldLabel(why: Held): string {
  const labels: Record<Held, string> = {
    view_only: "You may only view it: not sent",
    upload_only: "You may only add files here: not changed on the server",
    hidden: "No longer shared with you: kept here",
    unavailable: "Its drive's storage is unavailable: sent once it is back",
    moving: "Its drive is being moved: sent once that is done",
  };
  return labels[why];
}

/** Why an item is not synced, in the server's words when it gave some. */
export function heldReason(item: NotSynced): string {
  return item.reason ?? heldLabel(item.why);
}

/** Why some synced folders wait for their drive's storage; null when none does. */
export function waitingNote(waiting: Waiting[]): string | null {
  const n = waiting.length;
  if (n === 0) return null;
  const which = n === 1 ? "1 synced folder waits" : `${n} synced folders wait`;
  return `${which} for ${n === 1 ? "its" : "their"} drive's storage: unavailable now, or being moved. Changes made here are kept and sent once it is back.`;
}

/** The title of what is not synced ("2 items not synced"); null when everything is. */
export function notSyncedTitle(notSynced: NotSynced[]): string | null {
  const n = notSynced.length;
  if (n === 0) return null;
  return n === 1 ? "1 item not synced" : `${n} items not synced`;
}

/** How a synced folder is kept, in a word. */
export function modeLabel(mode: FolderMode): string {
  return mode === "on_demand" ? "On-demand" : "Sync";
}

/** Why encrypted folders are not synced, if some are not; null when all are. */
export function encryptionNote(encryption: Encryption): string | null {
  const n = encryption.skipped;
  if (n === 0) return null;
  const folders = n === 1 ? "1 encrypted folder is" : `${n} encrypted folders are`;
  return encryption.unlocked
    ? `${folders} not synced: you have no key for ${n === 1 ? "it" : "them"} yet.`
    : `${folders} not synced until you unlock encryption on this computer.`;
}

/** "3 folders", "1 folder". */
export function folderCount(n: number): string {
  return n === 1 ? "1 folder" : `${n} folders`;
}

/** The server's host, for a line of text: `files.example.com`. */
export function host(server: string | null): string {
  if (!server) return "";
  return server.replace(/^[a-z]+:\/\//i, "");
}

/** The patterns of the ignore list as typed, one per line. */
export function parsePatterns(text: string): string[] {
  return text
    .split("\n")
    .map((line) => line.trim())
    .filter(Boolean);
}
