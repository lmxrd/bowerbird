import { describe, expect, it } from "vitest";
import type { TeamMember } from "organizer-sdk";
import { NO_TEAM, chosenTeam, emailsOf, teamOptions, teamSummary } from "./teams";

describe("teams", () => {
  it("say their members and storage", () => {
    const members = [{ email: "a@example.com" }, { email: "b@example.com" }] as TeamMember[];
    expect(teamSummary({ members, used: 0, storage_limit: null })).toBe("2 members · 0 B");
    expect(teamSummary({ members: members.slice(1), used: 0, storage_limit: 1_000_000_000 })).toBe(
      "1 member · 0 B of 1.0 GB",
    );
  });
  it("take several emails at once", () => {
    expect(emailsOf("a@example.com, b@example.com\nc@example.com a@example.com")).toEqual([
      "a@example.com",
      "b@example.com",
      "c@example.com",
    ]);
    expect(emailsOf("  ")).toEqual([]);
  });
});

describe("teamOptions", () => {
  it("offers no team, then the teams by name", () => {
    expect(
      teamOptions([
        { id: "2", name: "research" },
        { id: "1", name: "Ops" },
      ]),
    ).toEqual([
      { value: NO_TEAM, label: "No team" },
      { value: "1", label: "Ops" },
      { value: "2", label: "research" },
    ]);
    expect(teamOptions(undefined)).toEqual([{ value: NO_TEAM, label: "No team" }]);
  });
  it("gives the chosen team's id, or none", () => {
    expect(chosenTeam("1")).toBe("1");
    expect(chosenTeam(NO_TEAM)).toBeUndefined();
  });
});
