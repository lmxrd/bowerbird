import { describe, expect, it } from "vitest";
import type { ActivityEntry } from "organizer-sdk";
import { activityActor, activityText } from "./activity";

const entry = (action: ActivityEntry["action"], detail: ActivityEntry["detail"] = {}) => ({
  action,
  detail,
});

describe("activityText", () => {
  it("says what was done to the account", () => {
    expect(activityText(entry("created"))).toBe("Made your account");
    expect(activityText(entry("storage_limit", { storage_limit: null }))).toBe(
      "Removed your storage limit",
    );
    expect(activityText(entry("storage_limit", { storage_limit: 10_000_000_000 }))).toMatch(
      /^Set your storage limit to 10/,
    );
    expect(activityText(entry("team_added", { team: "Staff", role: "manager" }))).toBe(
      "Added you to Staff as a manager",
    );
    expect(activityText(entry("team_added", { team: "Staff", role: "member" }))).toBe(
      "Added you to Staff",
    );
    expect(activityText(entry("team_role", { team: "Staff", role: "member" }))).toBe(
      "Made you a member of Staff",
    );
    expect(activityText(entry("drive_access", { drive: "Projects", role: "editor" }))).toBe(
      "Gave you editor access to Projects",
    );
    expect(activityText(entry("drive_access", { drive: "Projects", role: null }))).toBe(
      "Took you out of Projects",
    );
  });
});

describe("activityActor", () => {
  const by = (kind: string, email: string | null, detail: ActivityEntry["detail"] = {}) => ({
    actor: { kind, email },
    detail,
  });
  it("names the superadmin, or the command line", () => {
    expect(activityActor(by("superadmin", "admin@example.com"))).toBe("admin@example.com");
    expect(activityActor(by("command_line", null))).toBe("Server command line");
    expect(activityActor(by("superadmin", null))).toBe("A superadmin, removed since");
  });
  it("names a team manager with their team", () => {
    expect(activityActor(by("team_manager", "ana@example.com", { team: "Lab" }))).toBe(
      "ana@example.com, manager of Lab",
    );
    expect(activityActor(by("team_manager", null, { team: "Lab" }))).toBe(
      "A manager of Lab, removed since",
    );
    expect(activityActor(by("team_manager", null))).toBe("A team manager, removed since");
  });
});
