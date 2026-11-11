import { describe, expect, it } from "vitest";
import {
  MY_DRIVE,
  byName,
  driveSummary,
  driveUrl,
  formatMembers,
  hitUrl,
  managedBy,
  splatPath,
  withMember,
} from "./drives";

const team = { id: "d-1", kind: "shared" as const, name: "Team" };

describe("driveUrl", () => {
  it("keeps My drive's URLs", () => {
    expect(driveUrl(MY_DRIVE, "/")).toBe("/");
    expect(driveUrl(MY_DRIVE, "/Documents/Taxes 2025")).toBe("/files/Documents/Taxes%202025");
  });
  it("puts a shared drive under /drives/<id>", () => {
    expect(driveUrl(team, "/")).toBe("/drives/d-1");
    expect(driveUrl(team, "")).toBe("/drives/d-1");
    expect(driveUrl(team, "/Plans/Q1 2027")).toBe("/drives/d-1/Plans/Q1%202027");
    expect(driveUrl({ id: "a@b/c", kind: "shared" }, "/x")).toBe("/drives/a%40b%2Fc/x");
  });
});

describe("driveSummary", () => {
  it("counts members and storage of a shared drive", () => {
    expect(driveSummary({ kind: "shared", members: 3, used: 1_200_000_000, limit: null })).toBe(
      "3 members · 1.2 GB",
    );
    expect(
      driveSummary({ kind: "shared", members: 1, used: 1_200_000_000, limit: 10_000_000_000 }),
    ).toBe("1 member · 1.2 GB of 10 GB");
  });
  it("is the size alone for My drive", () => {
    expect(driveSummary({ kind: "my", members: 1, used: 840_000 })).toBe("840 KB");
  });
});

describe("formatMembers", () => {
  it("is singular for one", () => {
    expect(formatMembers(1)).toBe("1 member");
    expect(formatMembers(0)).toBe("0 members");
    expect(formatMembers(12)).toBe("12 members");
  });
});

describe("managedBy", () => {
  it("lists the managers", () => {
    expect(managedBy(["ana@example.com", "bo@example.com"])).toBe(
      "managed by ana@example.com, bo@example.com",
    );
    expect(managedBy([])).toBe("");
  });
});

describe("byName", () => {
  it("sorts as people read", () => {
    expect(byName(["b", "Äb", "a", "C"], (s) => s)).toEqual(["a", "Äb", "b", "C"]);
  });
});

describe("hitUrl", () => {
  const drives = [{ ...MY_DRIVE, id: "my-1" }, team];
  it("opens a folder in its drive", () => {
    expect(hitUrl({ kind: "folder", name: "Q1", path: "/Plans", drive_id: "d-1" }, drives)).toBe(
      "/drives/d-1/Plans/Q1",
    );
    expect(hitUrl({ kind: "file", name: "a.pdf", path: "/Taxes", drive_id: "my-1" }, drives)).toBe(
      "/files/Taxes",
    );
    expect(hitUrl({ kind: "file", name: "a.pdf", path: "/", drive_id: "d-1" }, drives)).toBe(
      "/drives/d-1",
    );
  });
  it("opens in My drive when the drive is not known", () => {
    expect(hitUrl({ kind: "file", name: "a", path: "/x", drive_id: "other" }, undefined)).toBe(
      "/files/x",
    );
  });
});

describe("withMember", () => {
  it("adds someone once", () => {
    const one = withMember([], " ana@example.com ", false);
    expect(one).toEqual([{ email: "ana@example.com", manages: false }]);
    expect(withMember(one, "ANA@example.com", true)).toEqual([
      { email: "ana@example.com", manages: true },
    ]);
    expect(withMember(one, "bo@example.com", false)).toHaveLength(2);
    expect(withMember(one, "  ", true)).toBe(one);
  });
});

describe("splatPath", () => {
  it("is a path from the drive's root", () => {
    expect(splatPath(undefined)).toBe("/");
    expect(splatPath("")).toBe("/");
    expect(splatPath("a/b/")).toBe("/a/b");
  });
});
