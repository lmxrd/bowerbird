import type { Role } from "organizer-sdk";

/** Roles from the least to the most (docs/M7B_DESIGN.md §1). */
export const ROLES: readonly Role[] = ["upload_only", "viewer", "editor", "manager", "owner"];

/** A role for people. */
export const ROLE_LABELS = {
  owner: "Owner",
  manager: "Manager",
  editor: "Editor",
  viewer: "Viewer",
  upload_only: "Upload only",
} as const satisfies Record<Role, string>;

/** Whether `role` is at least `least`; never for no role. */
export function atLeast(role: Role | null | undefined, least: Role): boolean {
  if (!role) return false;
  return ROLES.indexOf(role) >= ROLES.indexOf(least);
}

/**
 * What a role lets one do with an item, as the menus offer it:
 * - `read`: open, download, versions (a viewer and above);
 * - `add`: upload files and make folders (upload only and above);
 * - `edit`: rename, move, delete to the trash, replace (an editor and above);
 * - `link`: public links (an editor and above, where the settings allow);
 * - `manage`: who has access, limited access (a manager and above);
 * - `own`: delete for good, change the owner (the owner).
 */
export type Action = "read" | "add" | "edit" | "link" | "manage" | "own";

const LEAST: Record<Exclude<Action, "add">, Role> = {
  read: "viewer",
  edit: "editor",
  link: "editor",
  manage: "manager",
  own: "owner",
};

/**
 * Whether `role` allows `action`. Adding is the one thing upload only may
 * do that a viewer may not.
 */
export function can(role: Role | null | undefined, action: Action): boolean {
  if (action === "add") return role === "upload_only" || atLeast(role, "editor");
  return atLeast(role, LEAST[action]);
}

/**
 * The roles a manager or owner may give in the Share dialog, the highest
 * first: never owner (that is handed over), never more than their own,
 * and upload only on folders alone.
 */
export function givableRoles(mine: Role | null | undefined, kind: "file" | "folder"): Role[] {
  return (["manager", "editor", "viewer", "upload_only"] as const).filter(
    (role) => (role !== "upload_only" || kind === "folder") && atLeast(mine, role),
  );
}

/** The highest of two roles (a person's own grant and their team's). */
export function highest(a: Role | null | undefined, b: Role | null | undefined): Role | null {
  if (!a) return b ?? null;
  if (!b) return a;
  return ROLES.indexOf(a) >= ROLES.indexOf(b) ? a : b;
}

/**
 * What may be done with a trash item, by its `my_role` (the user's role on
 * its drive): restoring takes an editor; deleting for good is the owner's
 * (My drive's user), on a shared drive its managers'.
 */
export function trashActions(role: Role | null | undefined): { restore: boolean; purge: boolean } {
  return { restore: can(role, "edit"), purge: atLeast(role, "manager") };
}
