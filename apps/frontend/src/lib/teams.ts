import type { Team, TeamRole } from "organizer-sdk";
import { queryClient } from "@/lib/api";
import { byName } from "@/lib/drives";
import { memberCount } from "@/lib/sharing";
import { formatUsage } from "@/lib/users";

/** Every list of teams (and what follows from them), fetched again after a change. */
export function refreshTeams() {
  for (const _id of [
    "listMyTeams",
    "listTeams",
    "getTeam",
    "listRecipients",
    "listMySettings",
    "listDrives",
    "getDriveAccess",
  ]) {
    void queryClient.invalidateQueries({ queryKey: [{ _id }] });
  }
}

/** A team role for people. */
export const TEAM_ROLES = [
  { value: "manager", label: "Manager" },
  { value: "member", label: "Member" },
] as const satisfies readonly { value: TeamRole; label: string }[];

export const TEAM_ROLE_LABELS: Record<TeamRole, string> = { manager: "Manager", member: "Member" };

/** A team's line: "3 members · 1.2 GB of 10 GB". */
export function teamSummary(team: Pick<Team, "members" | "used" | "storage_limit">): string {
  return `${memberCount(team.members.length)} · ${formatUsage(team.used, team.storage_limit)}`;
}

/** The emails typed in a field, one or several (commas, spaces or lines between them). */
export function emailsOf(text: string): string[] {
  return [
    ...new Set(
      text
        .split(/[\s,;]+/)
        .map((e) => e.trim())
        .filter(Boolean),
    ),
  ];
}

/** The value of "No team" in a select of teams. */
export const NO_TEAM = "none";

/** A select of the team a drive belongs to: "No team", then the teams by name. */
export function teamOptions(teams: Pick<Team, "id" | "name">[] | undefined) {
  return [
    { value: NO_TEAM, label: "No team" },
    ...byName(teams ?? [], (t) => t.name).map((t) => ({ value: t.id, label: t.name })),
  ];
}

/** A team chosen in such a select, for the API: its id, or none. */
export function chosenTeam(value: string): string | undefined {
  return value === NO_TEAM ? undefined : value;
}
