import { describe, expect, it } from "vitest";
import {
  SCHEDULES,
  STATUS_LABELS,
  ejectName,
  nameTaken,
  lastCopyText,
  mayMirror,
  mirrorTitle,
  progressText,
  refusalText,
  restoreAction,
  scheduleLabel,
  sealedAs,
  statusBanner,
  statusVariant,
  stoppedText,
  stoppedWhen,
  trashSealedAs,
} from "./mirrors";
import { formatDay } from "./format";

describe("schedules", () => {
  it("offers continuous first, as the default", () => {
    expect(SCHEDULES[0].value).toBe("continuous");
    expect(SCHEDULES.map((s) => s.label)).toEqual([
      "Continuously",
      "Every hour",
      "Every day",
      "Only when I ask",
    ]);
  });
  it("names each schedule", () => {
    expect(scheduleLabel("hourly")).toBe("Every hour");
    expect(scheduleLabel("manual")).toBe("Only when I ask");
  });
});

describe("status", () => {
  it("labels every status", () => {
    expect(STATUS_LABELS.up_to_date).toBe("Up to date");
    expect(STATUS_LABELS.waiting_for_unlock).toBe("Waiting for unlock");
  });
  it("makes what needs the user stand out", () => {
    expect(statusVariant("failing")).toBe("destructive");
    expect(statusVariant("stopped")).toBe("destructive");
    expect(statusVariant("up_to_date")).toBe("secondary");
    expect(statusVariant("copying")).toBe("outline");
  });
  it("says why a mirror stopped", () => {
    expect(stoppedText("drive_deleted")).toBe("Stopped: the drive was deleted");
    expect(stoppedText("forbidden")).toBe("Stopped: the drive's managers don't allow mirroring it");
  });
});

describe("statusBanner", () => {
  it("keeps a deleted drive's copy", () => {
    expect(
      statusBanner({ status: "stopped", stopped: "drive_deleted", stopped_at: null, error: null }),
    ).toBe("Mirroring stopped: the drive was deleted. This copy stays until you remove it.");
  });
  it("goes on once another reason goes", () => {
    expect(
      statusBanner({
        status: "stopped",
        stopped: "shared_drives_off",
        stopped_at: null,
        error: null,
      }),
    ).toMatch(/isn't allowed here\. It goes on once that changes\./);
  });
  it("tells a failure and a wait", () => {
    expect(
      statusBanner({
        status: "failing",
        stopped: null,
        stopped_at: null,
        error: "Not enough room.",
      }),
    ).toBe("The last copy failed: Not enough room. It is tried again in a few minutes.");
    expect(
      statusBanner({ status: "waiting_for_unlock", stopped: null, stopped_at: null, error: null }),
    ).toMatch(/^Waiting for unlock/);
  });
  it("is quiet otherwise", () => {
    expect(
      statusBanner({ status: "up_to_date", stopped: null, stopped_at: null, error: null }),
    ).toBeUndefined();
    expect(
      statusBanner({ status: "copying", stopped: null, stopped_at: null, error: null }),
    ).toBeUndefined();
  });
});

describe("progress and last copy", () => {
  const progress = { files_done: 1204, files_total: 3880, bytes_done: 0, bytes_total: 0 };
  it("counts the files being copied", () => {
    expect(progressText(progress)).toBe("copying 1,204 of 3,880 files");
    expect(progressText({ ...progress, files_done: 0, files_total: 1 })).toBe(
      "copying 0 of 1 file",
    );
  });
  it("tells the progress while copying, else the last copy", () => {
    const now = new Date("2026-11-10T12:00:00Z");
    expect(lastCopyText({ status: "copying", progress, last_copy_at: null }, now)).toBe(
      "copying 1,204 of 3,880 files",
    );
    expect(lastCopyText({ status: "pending", progress: null, last_copy_at: null }, now)).toBe(
      "not copied yet",
    );
    expect(
      lastCopyText(
        { status: "up_to_date", progress: null, last_copy_at: "2026-11-02T10:00:00Z" },
        now,
      ),
    ).toMatch(/^last copied /);
  });
  it("titles a mirror's page", () => {
    expect(mirrorTitle("Team")).toBe("Team · mirror");
  });
});

describe("restoreAction", () => {
  it("restores into the drive when the user may write there", () => {
    expect(restoreAction({ restores_into_drive: true, stopped: null }, "file")).toBe("restore");
    expect(restoreAction({ restores_into_drive: true, stopped: "forbidden" }, "folder")).toBe(
      "restore",
    );
  });
  it("downloads a file otherwise; a folder has neither", () => {
    expect(restoreAction({ restores_into_drive: false, stopped: null }, "file")).toBe("download");
    expect(restoreAction({ restores_into_drive: false, stopped: null }, "folder")).toBeUndefined();
  });
  it("downloads once the drive is gone or no longer the user's", () => {
    expect(restoreAction({ restores_into_drive: true, stopped: "drive_deleted" }, "file")).toBe(
      "download",
    );
    expect(restoreAction({ restores_into_drive: true, stopped: "access_lost" }, "file")).toBe(
      "download",
    );
  });
});

describe("refusalText", () => {
  it("words a refusal by its code, not its message", () => {
    expect(refusalText({ error: "anything", code: "shared_drives_off" })).toMatch(
      /^Mirroring shared drives isn't allowed here/,
    );
    expect(refusalText({ error: "x", code: "mirroring_forbidden" })).toBe(
      "The drive's managers don't allow mirroring it.",
    );
    expect(refusalText({ error: "x", code: "drive_read_only" })).toMatch(/download it instead/);
  });
  it("leaves others to the usual message", () => {
    expect(refusalText({ error: "x" })).toBeUndefined();
    expect(refusalText({ error: "x", code: "something_else" })).toBeUndefined();
    expect(refusalText(new Error("x"))).toBeUndefined();
    expect(refusalText(undefined)).toBeUndefined();
  });
});

describe("mayMirror", () => {
  it("needs storage of the user's own", () => {
    expect(mayMirror({ kind: "my", my_role: "owner" }, 0, undefined)).toBe(false);
    expect(mayMirror({ kind: "my", my_role: "owner" }, 1, undefined)).toBe(true);
  });
  it("lets a viewer and above mirror a shared drive its managers allow", () => {
    expect(mayMirror({ kind: "shared", my_role: "viewer" }, 1, true)).toBe(true);
    expect(mayMirror({ kind: "shared", my_role: "editor" }, 1, undefined)).toBe(true);
    expect(mayMirror({ kind: "shared", my_role: "upload_only" }, 1, true)).toBe(false);
    expect(mayMirror({ kind: "shared", my_role: "manager" }, 1, false)).toBe(false);
  });
  it("never mirrors a mirror", () => {
    expect(mayMirror({ kind: "mirror", my_role: "viewer" }, 1, true)).toBe(false);
  });
});

describe("ejectName", () => {
  it("keeps the mirror's name when it is free", () => {
    expect(ejectName("My drive", ["Team"])).toEqual({ name: "My drive", needed: false });
  });
  it("finds a free one when it is taken", () => {
    expect(ejectName("Team", ["team"])).toEqual({ name: "Team (copy)", needed: true });
    expect(ejectName("Team", ["Team", "Team (copy)"])).toEqual({
      name: "Team (copy 2)",
      needed: true,
    });
  });
});

describe("sealedAs", () => {
  it("gives an encrypted node of a mirror the drive's ids", () => {
    const node = { id: "m-2", parent_id: "m-1", e2e_root: "m-1", source_id: "d-2", name: "x" };
    expect(sealedAs(node, "d-1", "d-1")).toEqual({
      id: "d-2",
      parent_id: "d-1",
      e2e_root: "d-1",
      source_id: "d-2",
      name: "x",
    });
  });
  it("keeps the top's own root", () => {
    const top = { id: "m-1", parent_id: "m-0", e2e_root: "m-1", source_id: "d-1" };
    expect(sealedAs(top, "d-0", "d-1")).toMatchObject({ id: "d-1", e2e_root: "d-1" });
  });
  it("opens a name with its parent's source, for a node that has none of its own yet", () => {
    // A file just put in a folder shared from another server.
    const fresh = { id: "b-3", parent_id: "b-1", e2e_root: "b-1", source_id: null };
    expect(sealedAs(fresh, "a-1", "a-1")).toEqual({
      id: "b-3",
      parent_id: "a-1",
      e2e_root: "a-1",
      source_id: null,
    });
  });
  it("leaves other nodes as they are", () => {
    const plain = { id: "n", parent_id: "p", e2e_root: null, source_id: "d" };
    expect(sealedAs(plain, "x", "y")).toBe(plain);
    const own = { id: "n", parent_id: "p", e2e_root: "r", source_id: null };
    expect(sealedAs(own, null, null)).toBe(own);
  });
});

describe("when a mirror stopped", () => {
  const now = new Date("2026-11-10T12:00:00Z");
  it("says the day the drive was deleted", () => {
    const at = "2026-11-02T09:30:00Z";
    expect(
      statusBanner(
        { status: "stopped", stopped: "drive_deleted", stopped_at: at, error: null },
        now,
      ),
    ).toBe(
      `Mirroring stopped: the drive was deleted on ${formatDay(at, now)}. This copy stays until you remove it.`,
    );
  });
  it("says today, and nothing when not known", () => {
    expect(stoppedWhen("2026-11-10T08:00:00Z", now)).toBe(" today");
    expect(stoppedWhen(null, now)).toBe("");
    expect(
      statusBanner(
        {
          status: "stopped",
          stopped: "forbidden",
          stopped_at: "2026-11-10T08:00:00Z",
          error: null,
        },
        now,
      ),
    ).toMatch(
      /^Mirroring stopped: the drive's managers don't allow mirroring it today\. It goes on/,
    );
  });
});

describe("eject's refusals", () => {
  it("words them by their code", () => {
    expect(refusalText({ error: "x", code: "copy_running" })).toMatch(/copying now/);
    expect(refusalText({ error: "x", code: "name_taken" })).toMatch(/has that name/);
  });
  it("asks for another name only when the name is taken", () => {
    expect(nameTaken({ error: "A shared drive has that name already", code: "name_taken" })).toBe(
      true,
    );
    expect(nameTaken({ error: "A shared drive has that name already" })).toBe(false);
    expect(nameTaken({ error: "x", code: "copy_running" })).toBe(false);
    expect(nameTaken(undefined)).toBe(false);
  });
});

describe("trashSealedAs", () => {
  const item = {
    id: "m-2",
    name: "sealed",
    original_parent_id: "m-1",
    e2e_root: "m-1",
    source_id: "d-2",
    original_parent_source_id: "d-1",
    e2e_root_source_id: "d-1",
  };
  it("opens a mirror's trashed name with the drive's ids", () => {
    expect(trashSealedAs(item)).toEqual({
      id: "d-2",
      name: "sealed",
      parent_id: "d-1",
      e2e_root: "d-1",
      source_id: "d-2",
    });
  });
  it("keeps another drive's item where it was", () => {
    const own = {
      ...item,
      source_id: null,
      original_parent_source_id: null,
      e2e_root_source_id: null,
    };
    expect(trashSealedAs(own)).toMatchObject({ id: "m-2", parent_id: "m-1", e2e_root: "m-1" });
  });
});
