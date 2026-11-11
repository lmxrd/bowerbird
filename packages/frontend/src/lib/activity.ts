import type { ActivityEntry, Role } from "organizer-sdk";
import { formatSize } from "@/lib/format";
import { ROLE_LABELS } from "@/lib/roles";

/** How many entries a page of the account activity has. */
export const ACTIVITY_PAGE = 25;

/** What was done to the account, for its owner: "Added you to Staff as a manager". */
export function activityText(entry: Pick<ActivityEntry, "action" | "detail">): string {
  const { team, drive, role, storage_limit } = entry.detail;
  const teamName = team ?? "a team";
  switch (entry.action) {
    case "created":
      return "Made your account";
    case "disabled":
      return "Disabled your account";
    case "enabled":
      return "Enabled your account";
    case "superadmin_granted":
      return "Made you a superadmin";
    case "superadmin_revoked":
      return "Took superadmin from you";
    case "storage_limit":
      return storage_limit == null
        ? "Removed your storage limit"
        : `Set your storage limit to ${formatSize(storage_limit)}`;
    case "files_folder":
      return "Changed your files folder";
    case "team_added":
      return role === "manager"
        ? `Added you to ${teamName} as a manager`
        : `Added you to ${teamName}`;
    case "team_removed":
      return `Took you out of ${teamName}`;
    case "team_role":
      return `Made you ${role === "manager" ? "a manager" : "a member"} of ${teamName}`;
    case "two_factor_reset":
      return "Reset your two-factor authentication";
    case "password_reset":
      return "Reset your password";
    case "drive_access": {
      const name = drive ?? "a shared drive";
      if (!role) return `Took you out of ${name}`;
      const label = ROLE_LABELS[role as Role] ?? role;
      return `Gave you ${label.toLowerCase()} access to ${name}`;
    }
    default:
      return entry.action;
  }
}

/**
 * Who did it: the superadmin's email, a team manager's ("ana@example.com,
 * manager of Lab"), or "Server command line".
 */
export function activityActor(entry: Pick<ActivityEntry, "actor" | "detail">): string {
  if (entry.actor.kind === "command_line") return "Server command line";
  if (entry.actor.kind === "team_manager") {
    const team = entry.detail.team;
    if (!entry.actor.email) {
      return team ? `A manager of ${team}, removed since` : "A team manager, removed since";
    }
    return team ? `${entry.actor.email}, manager of ${team}` : entry.actor.email;
  }
  return entry.actor.email ?? "A superadmin, removed since";
}
