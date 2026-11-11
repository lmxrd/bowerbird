import { describe, expect, it } from "vitest";
import type { Backend, BackendChoice } from "organizer-sdk";
import { errorMessage } from "./api";
import {
  DISK_ID,
  backendChanges,
  backendDetail,
  backendForm,
  backendState,
  backupNote,
  choiceLabel,
  choiceOptions,
  chosenBackend,
  formProblem,
  joinBucket,
  listEveryLabel,
  listEveryOptions,
  movingText,
  moveTargets,
  newBackendBody,
  ownStorageWarning,
  splitBucket,
  storedOn,
  unavailableText,
} from "./backends";

const backend: Backend = {
  id: "b-1",
  kind: "s3",
  name: "Backblaze",
  endpoint: "https://s3.eu-central-003.backblazeb2.com",
  region: "eu-central-003",
  bucket: "acme",
  prefix: "organizer",
  access_key: "key-1",
  secret_set: true,
  private: false,
  sealed_secret: null,
  limit: 2_000_000_000_000,
  drive_limit: null,
  used: 0,
  drives: 2,
  make_drives: "team_managers",
  team_ids: [],
  list_every_minutes: 60,
  owner: null,
  status: "ok",
  error: null,
  backed_up: false,
  created_at: null,
};

const choice = (over: Partial<BackendChoice>): BackendChoice => ({
  id: "b-1",
  kind: "s3",
  name: "Backblaze",
  owner: null,
  backed_up: false,
  available: true,
  error: null,
  ...over,
});

describe("backendState", () => {
  it("says whether the drives can be used", () => {
    expect(backendState({ status: "ok", private: false })).toEqual({
      label: "Connected",
      tone: "ok",
    });
    expect(backendState({ status: "failing", private: false }).label).toBe("Unreachable");
    expect(backendState({ status: "locked", private: true })).toEqual({
      label: "Locked",
      tone: "locked",
    });
    expect(backendState({ status: "ok", private: true }).label).toBe("Unlocked");
  });
});

describe("backendDetail", () => {
  it("says where, how many drives and whether it is backed up", () => {
    expect(backendDetail(backend)).toBe("acme/organizer · 2 drives · Not backed up by Organizer");
    expect(backendDetail({ bucket: null, prefix: null, drives: 1, backed_up: true })).toBe(
      "1 drive · Backed up",
    );
  });
});

describe("buckets", () => {
  it("splits a bucket from its prefix", () => {
    expect(splitBucket("acme")).toEqual({ bucket: "acme", prefix: "" });
    expect(splitBucket(" /acme/organizer/drives/ ")).toEqual({
      bucket: "acme",
      prefix: "organizer/drives",
    });
    expect(joinBucket("acme", "organizer")).toBe("acme/organizer");
    expect(joinBucket("acme", "")).toBe("acme");
    expect(joinBucket(null, null)).toBe("");
  });
});

describe("look for changes", () => {
  it("names the intervals, and keeps one set elsewhere", () => {
    expect(listEveryLabel(60)).toBe("Every hour");
    expect(listEveryLabel(0)).toBe("Never");
    expect(listEveryLabel(90)).toBe("Every 90 minutes");
    expect(listEveryOptions("60").some((o) => o.value === "90")).toBe(false);
    expect(listEveryOptions("90").at(-1)).toEqual({ value: "90", label: "Every 90 minutes" });
  });
});

describe("the form", () => {
  it("starts empty, hourly, made by the superadmin", () => {
    const form = backendForm();
    expect(form.listEvery).toBe("60");
    expect(form.makeDrives).toBe("superadmin");
    expect(formProblem(form, true)).toBe("A name is needed");
  });

  it("asks for a secret when adding only, and for teams when chosen", () => {
    const form = backendForm(backend);
    expect(formProblem(form, false)).toBeUndefined();
    expect(formProblem(form, true)).toBe("A secret is needed");
    expect(formProblem({ ...form, makeDrives: "teams" }, false)).toBe("Choose at least one team");
    expect(formProblem({ ...form, limit: "lots" }, false)).toMatch(/GB/);
  });

  it("makes a global backend's body, and a user's own without who makes drives", () => {
    const form = {
      ...backendForm(backend),
      secret: "s",
      limit: "1.5",
      makeDrives: "teams" as const,
    };
    const global = newBackendBody({ ...form, teamIds: ["t-1"] }, false);
    expect(global).toMatchObject({
      bucket: "acme",
      prefix: "organizer",
      limit: 1_500_000_000,
      drive_limit: null,
      list_every_minutes: 60,
      make_drives: "teams",
      team_ids: ["t-1"],
    });
    expect(global.private).toBeUndefined();
    const own = newBackendBody({ ...form, private: true }, true);
    expect(own.make_drives).toBeUndefined();
    expect(own.private).toBe(true);
  });

  it("sends only what changed, and a secret only when typed", () => {
    const form = backendForm(backend);
    expect(backendChanges(form, backend, false)).toEqual({});
    expect(backendChanges({ ...form, secret: "new", listEvery: "0" }, backend, false)).toEqual({
      secret: "new",
      list_every_minutes: 0,
    });
    expect(backendChanges({ ...form, bucket: "other" }, backend, false)).toEqual({
      bucket: "other",
      prefix: "",
    });
    expect(backendChanges({ ...form, makeDrives: "everyone" }, backend, true)).toEqual({});
  });
});

describe("Stored on", () => {
  it("marks the user's own storage and what cannot be used", () => {
    expect(choiceLabel(choice({}))).toBe("Backblaze");
    expect(choiceLabel(choice({ owner: "ana@example.com", name: "Mine" }))).toBe(
      "Mine (your storage)",
    );
    expect(choiceLabel(choice({ available: false }))).toBe("Backblaze: unavailable");
    expect(choiceOptions([choice({ available: false })])[0].disabled).toBe(true);
  });

  it("says when the server's backups do not cover it", () => {
    expect(backupNote(choice({ backed_up: true }))).toBeUndefined();
    expect(backupNote(choice({}))).toBe(
      "Not in the server's backups. Use the provider's versioning.",
    );
    expect(backupNote(undefined)).toBeUndefined();
  });

  it("sends nothing for the server's disk", () => {
    expect(chosenBackend(DISK_ID)).toBeUndefined();
    expect(chosenBackend("")).toBeUndefined();
    expect(chosenBackend("b-1")).toBe("b-1");
  });

  it("offers to move a drive anywhere but where it is", () => {
    const targets = moveTargets([choice({ id: DISK_ID, name: "Server" }), choice({})], {
      backend: { ...choice({}) },
    });
    expect(targets.map((t) => t.value)).toEqual([DISK_ID]);
  });
});

describe("a drive's storage", () => {
  const disk = { id: DISK_ID, kind: "disk" as const, name: "Server", owner: null, backed_up: true };
  const own = {
    ...disk,
    id: "b-2",
    kind: "s3" as const,
    owner: "ana@example.com",
    backed_up: false,
  };

  it("warns of someone's own storage", () => {
    expect(ownStorageWarning({ backend: disk })).toBeUndefined();
    expect(ownStorageWarning({ backend: own })).toBe(
      "Stored on ana@example.com's own storage. If they remove it, this drive goes. The server doesn't back it up.",
    );
    expect(ownStorageWarning({ backend: own }, "ana@example.com")).toMatch(/^Stored on your own/);
  });

  it("says where it is, where it goes and why it cannot be used", () => {
    expect(storedOn({ backend: disk })).toBe("Stored on the server");
    expect(storedOn({ backend: { ...own, name: "Ana's B2" } })).toBe("Stored on Ana's B2");
    expect(movingText({ moving_to: null })).toBeUndefined();
    expect(movingText({ moving_to: { ...own, name: "B2" } })).toBe("Moving to B2…");
    expect(unavailableText({ available: true, unavailable_reason: null })).toBeUndefined();
    expect(
      unavailableText({
        available: false,
        unavailable_reason: "Locked until ana@example.com signs in",
      }),
    ).toBe("Locked until ana@example.com signs in");
  });
});

describe("errors", () => {
  it("says a storage that is down changed nothing", () => {
    expect(
      errorMessage({ error: "The storage cannot be reached", code: "storage_unavailable" }),
    ).toBe("The storage cannot be reached. Nothing was changed: try again once it's back.");
    expect(errorMessage({ error: "Read-only" })).toBe("Read-only");
  });

  it("says a drive being moved changed nothing, by its code", () => {
    expect(errorMessage({ error: "Moved, read-only", code: "read_only" })).toBe(
      "This drive is being moved to other storage. Nothing was changed: try again once it's done.",
    );
  });
});
