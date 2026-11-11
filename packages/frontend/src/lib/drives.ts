import { folderUrl } from "@/lib/format";
import { formatUsage } from "@/lib/users";

/**
 * What the pages need to know of a drive to link into it and name it: My
 * drive (the user's own tree, at `/` and `/files/<path>`) or a shared drive
 * (at `/drives/<id>` and `/drives/<id>/<path>`).
 */
export type DriveRef = { id: string; kind: "my" | "shared" | "mirror" | "remote"; name: string };

/** My drive while the list of drives is not there yet: its URLs need no id. */
export const MY_DRIVE: DriveRef = { id: "", kind: "my", name: "My drive" };

/**
 * The URL of a folder of a drive, by its path from the drive's root. My
 * drive keeps the URLs it had before there were drives.
 */
export function driveUrl(drive: Pick<DriveRef, "id" | "kind">, path: string): string {
  if (drive.kind === "my") return folderUrl(path);
  const parts = path.split("/").filter(Boolean);
  return [`/drives/${encodeURIComponent(drive.id)}`, ...parts.map(encodeURIComponent)].join("/");
}

/** "1 member", "3 members". */
export function formatMembers(count: number): string {
  return `${count} ${count === 1 ? "member" : "members"}`;
}

/**
 * The line under a drive's name: "3 members · 1.2 GB of 10 GB" for a
 * shared drive, the size alone for My drive (whose one member is the user).
 */
export function driveSummary(drive: {
  kind: "my" | "shared" | "mirror" | "remote";
  members: number;
  used: number;
  limit?: number | null;
}): string {
  const size = formatUsage(drive.used, drive.limit);
  return drive.kind === "my" ? size : `${formatMembers(drive.members)} · ${size}`;
}

/** "managed by ana@example.com, bo@example.com"; empty when nobody does. */
export function managedBy(managers: string[]): string {
  return managers.length ? `managed by ${managers.join(", ")}` : "";
}

/** Drives by name, as people read them (case and accents aside). */
export function byName<T>(items: T[], name: (item: T) => string): T[] {
  return [...items].sort((a, b) =>
    name(a).localeCompare(name(b), undefined, { sensitivity: "base" }),
  );
}

/**
 * Where a search hit opens: the folder it is (or is in), in its own drive.
 * A hit of a drive the user does not have in the list (yet) opens in My
 * drive, as before there were drives.
 */
export function hitUrl(
  hit: { kind: string; name: string; path: string; drive_id: string },
  drives: DriveRef[] | undefined,
): string {
  const parent = hit.path.split("/").filter(Boolean);
  const path = `/${[...parent, ...(hit.kind === "folder" ? [hit.name] : [])].join("/")}`;
  const drive = drives?.find((d) => d.id === hit.drive_id);
  return driveUrl(drive ?? MY_DRIVE, path);
}

/** The path of a drive's folder from the splat of its URL: `a/b` → `/a/b`. */
export function splatPath(splat: string | undefined): string {
  return `/${(splat ?? "").split("/").filter(Boolean).join("/")}`;
}

/** A member named in the "New shared drive" dialog, before the drive exists. */
export type NewMember = { email: string; manages: boolean };

/**
 * The members of a new drive with one more: someone named again (any case)
 * is not added twice, only whether they manage it changes.
 */
export function withMember(members: NewMember[], email: string, manages: boolean): NewMember[] {
  const trimmed = email.trim();
  if (!trimmed) return members;
  const same = (m: NewMember) => m.email.toLowerCase() === trimmed.toLowerCase();
  if (members.some(same)) return members.map((m) => (same(m) ? { ...m, manages } : m));
  return [...members, { email: trimmed, manages }];
}
