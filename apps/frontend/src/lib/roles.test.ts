import { describe, expect, it } from "vitest";
import { atLeast, can, givableRoles, highest, trashActions } from "./roles";

describe("roles", () => {
  it("are ordered from upload only to owner", () => {
    expect(atLeast("manager", "editor")).toBe(true);
    expect(atLeast("viewer", "editor")).toBe(false);
    expect(atLeast(null, "upload_only")).toBe(false);
    expect(highest("viewer", "editor")).toBe("editor");
    expect(highest(null, "viewer")).toBe("viewer");
    expect(highest(undefined, null)).toBeNull();
  });
  it("allow what the menus offer", () => {
    expect(can("viewer", "read")).toBe(true);
    expect(can("viewer", "edit")).toBe(false);
    expect(can("viewer", "add")).toBe(false);
    expect(can("upload_only", "add")).toBe(true);
    expect(can("upload_only", "read")).toBe(false);
    expect(can("editor", "add")).toBe(true);
    expect(can("editor", "link")).toBe(true);
    expect(can("editor", "manage")).toBe(false);
    expect(can("manager", "own")).toBe(false);
    expect(can("owner", "own")).toBe(true);
    expect(can(null, "read")).toBe(false);
  });
  it("give at most one's own, never owner, upload only on folders", () => {
    expect(givableRoles("owner", "folder")).toEqual(["manager", "editor", "viewer", "upload_only"]);
    expect(givableRoles("owner", "file")).toEqual(["manager", "editor", "viewer"]);
    expect(givableRoles("editor", "folder")).toEqual(["editor", "viewer", "upload_only"]);
    expect(givableRoles(null, "folder")).toEqual([]);
  });
});

describe("trashActions", () => {
  it("lets an editor restore, and the owner or a manager delete for good", () => {
    expect(trashActions("owner")).toEqual({ restore: true, purge: true });
    expect(trashActions("manager")).toEqual({ restore: true, purge: true });
    expect(trashActions("editor")).toEqual({ restore: true, purge: false });
    expect(trashActions("viewer")).toEqual({ restore: false, purge: false });
    expect(trashActions("upload_only")).toEqual({ restore: false, purge: false });
    expect(trashActions(null)).toEqual({ restore: false, purge: false });
  });
});
