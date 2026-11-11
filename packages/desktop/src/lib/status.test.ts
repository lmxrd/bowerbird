import { describe, expect, it } from "vitest";
import type { Status } from "./desktop";
import {
  activityLabel,
  encryptionNote,
  folderCount,
  headline,
  heldLabel,
  heldReason,
  host,
  modeLabel,
  notSyncedTitle,
  parsePatterns,
  progress,
  waitingNote,
} from "./status";

const status = (over: Partial<Status> = {}): Status => ({
  phase: "up_to_date",
  pending: 0,
  done: 0,
  folders: 1,
  recent: [],
  conflicts: [],
  error: null,
  synced_at: null,
  encryption: { set_up: false, unlocked: false, skipped: 0 },
  unavailable: [],
  not_synced: [],
  waiting: [],
  ...over,
});

describe("waiting for storage", () => {
  it("says why folders wait, and each item's reason in the server's words", () => {
    expect(waitingNote([])).toBeNull();
    const locked = "Locked until alice@example.com signs in";
    expect(waitingNote([{ folder: 1, why: "unavailable", reason: locked }])).toMatch(
      /^1 synced folder waits for its drive's storage/,
    );
    expect(
      waitingNote([
        { folder: 1, why: "unavailable", reason: locked },
        { folder: 2, why: "moving", reason: "Being moved" },
      ]),
    ).toMatch(/^2 synced folders wait for their drive's storage/);
    expect(heldReason({ path: "Team/a.txt", why: "unavailable", reason: locked })).toBe(locked);
    expect(heldReason({ path: "Team/a.txt", why: "moving" })).toBe(
      "Its drive is being moved: sent once that is done",
    );
  });
});

describe("not synced", () => {
  it("counts what is kept here and says why", () => {
    expect(notSyncedTitle([])).toBeNull();
    expect(notSyncedTitle([{ path: "Team/a.txt", why: "view_only" }])).toBe("1 item not synced");
    expect(
      notSyncedTitle([
        { path: "Team/a.txt", why: "view_only" },
        { path: "Team/Secret", why: "hidden" },
      ]),
    ).toBe("2 items not synced");
    expect(heldLabel("view_only")).toBe("You may only view it: not sent");
    expect(heldLabel("hidden")).toBe("No longer shared with you: kept here");
    expect(activityLabel("restore")).toBe("Brought back: not yours to delete");
  });
});

describe("headline", () => {
  it("says what the engine does", () => {
    expect(headline(status(), false)).toBe("Up to date");
    expect(headline(status({ phase: "syncing", pending: 3 }), false)).toBe("Syncing 3");
    expect(headline(status({ phase: "syncing" }), false)).toBe("Up to date");
    expect(headline(status({ phase: "syncing", pending: 3 }), true)).toBe("Paused");
    expect(headline(status({ phase: "offline" }), false)).toBe("Offline");
    expect(headline(status({ phase: "error", error: "Boom" }), false)).toBe("Error: Boom");
  });
});

describe("encryptionNote", () => {
  it("says why encrypted folders wait, if they do", () => {
    expect(encryptionNote(status().encryption)).toBeNull();
    expect(encryptionNote({ set_up: true, unlocked: false, skipped: 1 })).toBe(
      "1 encrypted folder is not synced until you unlock encryption on this computer.",
    );
    expect(encryptionNote({ set_up: true, unlocked: true, skipped: 2 })).toBe(
      "2 encrypted folders are not synced: you have no key for them yet.",
    );
  });
});

describe("progress", () => {
  it("is the share done of a round under way", () => {
    expect(progress(status())).toBeNull();
    expect(progress(status({ phase: "syncing", pending: 3, done: 1 }))).toBe(25);
  });
});

describe("the small words", () => {
  it("counts folders and shortens the server", () => {
    expect(folderCount(1)).toBe("1 folder");
    expect(folderCount(0)).toBe("0 folders");
    expect(host("https://files.example.com")).toBe("files.example.com");
    expect(parsePatterns(" *.tmp \n\n.DS_Store\n")).toEqual(["*.tmp", ".DS_Store"]);
    expect(modeLabel("sync")).toBe("Sync");
    expect(modeLabel("on_demand")).toBe("On-demand");
  });
});
