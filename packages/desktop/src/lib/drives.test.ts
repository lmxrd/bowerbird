import { describe, expect, it } from "vitest";
import type { Drive } from "organizer-sdk/types";
import type { SyncedFolder } from "./desktop";
import {
  defaultPlace,
  driveLabel,
  driveRows,
  driveSummary,
  folderRole,
  mirrorState,
  remoteOrigin,
  remoteState,
  roleLabel,
  roleNote,
  storageKind,
  storageNotes,
  storageState,
  syncable,
  unavailableNote,
} from "./drives";

const drive = (id: string, over: Partial<Drive> = {}): Drive => ({
  id,
  kind: "shared",
  name: id,
  root_id: `${id}-root`,
  used: 0,
  limit: null,
  manages: false,
  members: 2,
  managers: [],
  my_role: "editor",
  team_id: null,
  team_name: null,
  backend: {
    id: "00000000-0000-0000-0000-000000000000",
    name: "The server's disk",
    kind: "disk",
    owner: null,
    backed_up: true,
  },
  available: true,
  unavailable: null,
  unavailable_reason: null,
  moving_to: null,
  mirror_of: null,
  remote: null,
  ...over,
});

const folder = (id: number, over: Partial<SyncedFolder> = {}): SyncedFolder => ({
  id,
  name: `folder ${id}`,
  local_path: `/home/anna/Organizer/folder ${id}`,
  mode: "sync",
  remote_id: `node-${id}`,
  drive_id: null,
  unavailable: false,
  waiting: null,
  role: null,
  ...over,
});

const my = drive("my", { kind: "my", name: "My drive", manages: true, members: 1 });
const team = drive("team", { name: "Team" });
const empty = drive("empty", { name: "Empty", root_id: null });

describe("driveRows", () => {
  it("puts each synced folder under its drive, a drive's root as the whole drive", () => {
    const whole = folder(1, { remote_id: "team-root", drive_id: "team" });
    const inTeam = folder(2, { drive_id: "team" });
    const { rows, others } = driveRows([my, team, empty], [whole, inTeam]);
    expect(rows.map((r) => r.drive.name)).toEqual(["My drive", "Team", "Empty"]);
    expect(rows[1].whole).toBe(whole);
    expect(rows[1].folders).toEqual([inTeam]);
    expect(rows[0]).toMatchObject({ whole: null, folders: [] });
    expect(others).toEqual([]);
  });

  it("takes a folder synced before drives for My drive's", () => {
    const top = folder(1, { remote_id: "my-root" });
    const docs = folder(2);
    const { rows } = driveRows([my, team], [top, docs]);
    expect(rows[0].whole).toBe(top);
    expect(rows[0].folders).toEqual([docs]);
  });

  it("keeps apart the folders of a drive no longer listed", () => {
    const left = folder(1, { remote_id: "gone-root", drive_id: "gone", unavailable: true });
    const { rows, others } = driveRows([my, team], [left]);
    expect(rows.every((r) => r.whole === null && r.folders.length === 0)).toBe(true);
    expect(others).toEqual([left]);
  });

  it("lists every folder apart while the drives are not known", () => {
    const docs = folder(1);
    expect(driveRows([], [docs])).toEqual({ rows: [], mirrors: [], remotes: [], others: [docs] });
  });

  it("puts the mirrors in a group of their own, with their synced folders", () => {
    const mirror = drive("mirror", { kind: "mirror", name: "Team", my_role: "viewer" });
    const synced = folder(1, { remote_id: "mirror-root", drive_id: "mirror" });
    const { rows, mirrors } = driveRows([my, team, mirror], [synced]);
    expect(rows.map((r) => r.drive.id)).toEqual(["my", "team"]);
    expect(mirrors.map((r) => r.drive.id)).toEqual(["mirror"]);
    expect(mirrors[0].whole).toBe(synced);
  });
});

describe("mirrors", () => {
  const mirror = (over: Partial<NonNullable<Drive["mirror_of"]>> = {}) =>
    drive("mirror", {
      kind: "mirror",
      name: "Team",
      my_role: "viewer",
      backend: {
        id: "b1",
        name: "Anna's bucket",
        kind: "s3",
        owner: "anna@example.com",
        backed_up: false,
      },
      mirror_of: {
        drive_id: "team",
        drive_name: "Team",
        schedule: "continuous",
        status: "up_to_date",
        stopped: null,
        stopped_at: null,
        restores_into_drive: true,
        ...over,
      },
    });

  it("labels a mirror as one, and no other drive", () => {
    expect(driveLabel(mirror())).toBe("Team · mirror");
    expect(driveLabel(team)).toBe("Team");
  });

  it("says where a mirror stands", () => {
    expect(mirrorState(team)).toBeNull();
    expect(mirrorState(mirror())).toBe("Up to date");
    expect(mirrorState(mirror({ status: "copying" }))).toBe("Copying…");
    expect(mirrorState(mirror({ status: "waiting_for_unlock" }))).toBe(
      "Waiting for your storage to be unlocked",
    );
    expect(mirrorState(mirror({ status: "failing" }))).toMatch(/^Copying fails/);
    expect(mirrorState(mirror({ status: "stopped", stopped: "drive_deleted" }))).toBe(
      "Stopped: The drive was deleted",
    );
    expect(mirrorState(mirror({ status: "stopped", stopped: "forbidden" }))).toBe(
      "Stopped: The drive's managers do not allow mirroring it",
    );
  });

  it("is read-only, and warns of no storage of the user's own", () => {
    expect(roleNote(mirror().my_role)).toMatch(/^View only/);
    expect(storageNotes(mirror())).toEqual([]);
  });
});

describe("defaultPlace", () => {
  const dir = "/home/anna/Organizer";

  it("is the name under the Organizer folder, the top without one as Files", () => {
    expect(defaultPlace(dir, "Team", [])).toBe(`${dir}/Team`);
    expect(defaultPlace(dir, "My drive", [])).toBe(`${dir}/My drive`);
    expect(defaultPlace(dir, "", [])).toBe(`${dir}/Files`);
  });

  it("gets a number where a synced folder or the drop box folder is", () => {
    const taken = [`${dir}/Drop box`, `${dir}/Team`, `${dir}/Team 2`, `${dir}/Files/Taxes`];
    expect(defaultPlace(dir, "Drop box", taken)).toBe(`${dir}/Drop box 2`);
    expect(defaultPlace(dir, "Team", taken)).toBe(`${dir}/Team 3`);
    expect(defaultPlace(dir, "Teams", taken)).toBe(`${dir}/Teams`);
    expect(defaultPlace(dir, "", taken)).toBe(`${dir}/Files 2`);
  });

  it("joins with the system's separator", () => {
    expect(defaultPlace("C:\\Users\\anna\\Organizer", "Team", [])).toBe(
      "C:\\Users\\anna\\Organizer\\Team",
    );
  });
});

describe("driveSummary", () => {
  it("says where a drive is, or how much of it is synced", () => {
    const whole = folder(1, { remote_id: "team-root", local_path: "/x/Team" });
    expect(driveSummary({ drive: team, whole, folders: [] })).toBe("/x/Team");
    expect(driveSummary({ drive: team, whole: null, folders: [] })).toBe("Not synced");
    expect(driveSummary({ drive: team, whole: null, folders: [folder(2)] })).toBe(
      "1 folder in it synced",
    );
    expect(driveSummary({ drive: team, whole: null, folders: [folder(2), folder(3)] })).toBe(
      "2 folders in it synced",
    );
    expect(driveSummary({ drive: empty, whole: null, folders: [] })).toBe(
      "Not available on the server now",
    );
  });
});

describe("roles", () => {
  it("names each role", () => {
    expect(roleLabel("owner")).toBe("Owner");
    expect(roleLabel("viewer")).toBe("Viewer");
    expect(roleLabel("upload_only")).toBe("Upload only");
  });

  it("says what a role holds back from syncing, if anything", () => {
    expect(roleNote("editor")).toBeNull();
    expect(roleNote(null)).toBeNull();
    expect(roleNote("viewer")).toBe("View only: what you change here is not sent.");
    expect(roleNote("upload_only")).toMatch(/^Upload only: new files are sent/);
  });

  it("shows a folder's own role only where it is not its drive's", () => {
    expect(folderRole(folder(1, { role: "editor" }), team)).toBeNull();
    expect(folderRole(folder(1, { role: "viewer" }), team)).toBe("viewer");
    expect(folderRole(folder(1), team)).toBeNull();
    expect(folderRole(folder(1, { role: "manager" }), null)).toBe("manager");
  });
});

describe("storage", () => {
  const bucket = {
    id: "b1",
    name: "Alice's bucket",
    kind: "s3" as const,
    owner: "alice@example.com",
    backed_up: false,
  };

  it("says nothing of a drive on the server's disk", () => {
    expect(storageNotes(team)).toEqual([]);
    expect(storageState(team)).toBeNull();
    expect(storageKind(team)).toBeNull();
  });

  it("warns of someone's own storage, and of no backups", () => {
    expect(storageNotes(drive("own", { backend: bucket }))).toEqual([
      "Stored on alice@example.com's own storage. If they remove it, this drive goes.",
      "Not backed up by the server.",
    ]);
    const global = { ...bucket, name: "S3", owner: null };
    expect(storageNotes(drive("global", { backend: global }))).toEqual([
      "Not backed up by the server.",
    ]);
  });

  it("says why a drive does not sync now: locked, failing or moving", () => {
    const locked = drive("locked", {
      available: false,
      unavailable: "locked",
      unavailable_reason: "Locked until alice@example.com signs in",
    });
    expect(storageKind(locked)).toBe("locked");
    expect(storageState(locked)).toBe(
      "Not available: Locked until alice@example.com signs in. Changes made here wait.",
    );
    const failing = drive("failing", {
      available: false,
      unavailable: "failing",
      unavailable_reason: "The storage cannot be reached",
    });
    expect(storageKind(failing)).toBe("unavailable");
    // By the server's field, not its words.
    const lockedInOtherWords = drive("other words", {
      available: false,
      unavailable: "locked",
      unavailable_reason: "Waiting for its owner",
    });
    expect(storageKind(lockedInOtherWords)).toBe("locked");
    const moving = drive("moving", { moving_to: bucket });
    expect(storageKind(moving)).toBe("moving");
    expect(storageState(moving)).toMatch(/^Moving to Alice's bucket… Read-only meanwhile/);
  });
});

describe("unavailableNote", () => {
  it("counts the folders not synced because they are gone", () => {
    expect(unavailableNote([folder(1)])).toBeNull();
    expect(unavailableNote([folder(1, { unavailable: true })])).toBe(
      "1 synced folder is no longer on the server, or you are no longer a member of its drive.",
    );
    expect(
      unavailableNote([folder(1, { unavailable: true }), folder(2, { unavailable: true })]),
    ).toMatch(/^2 synced folders are .* their drive\.$/);
  });
});

describe("drives from other servers", () => {
  const remote = (over: Partial<NonNullable<Drive["remote"]>> = {}) =>
    drive("remote", {
      kind: "remote",
      name: "Plans",
      my_role: "viewer",
      remote: {
        server: "cloud.example",
        owner: "alice@cloud.example",
        owner_name: "Alice",
        shared_by: "alice@cloud.example",
        state: "accepted",
        error: null,
        ...over,
      },
    });

  it("are grouped apart, after the mirrors", () => {
    const { rows, mirrors, remotes } = driveRows([my, team, remote()], []);
    expect(rows.map((r) => r.drive.id)).toEqual(["my", "team"]);
    expect(mirrors).toEqual([]);
    expect(remotes.map((r) => r.drive.id)).toEqual(["remote"]);
  });

  it("say whose they are and where", () => {
    expect(remoteOrigin(team)).toBeNull();
    expect(remoteOrigin(remote())).toBe("From alice@cloud.example");
    expect(remoteOrigin(remote({ owner: "alice@example.com", server: "files.example.org" }))).toBe(
      "From alice@example.com on files.example.org",
    );
  });

  it("say where they stand, and sync once accepted", () => {
    expect(remoteState(team)).toBeNull();
    expect(remoteState(remote())).toBeNull();
    expect(remoteState(remote({ state: "pending" }))).toMatch(/accept it/);
    expect(remoteState(remote({ error: "unreachable" }))).toBe(
      "Its server does not answer now: unreachable",
    );
    expect(syncable(remote())).toBe(true);
    expect(syncable(remote({ state: "pending" }))).toBe(false);
    expect(syncable(team)).toBe(true);
    expect(syncable(empty)).toBe(false);
  });

  it("follow their role, and warn of no storage here", () => {
    expect(roleNote(remote().my_role)).toMatch(/^View only/);
    expect(storageNotes(remote())).toEqual([]);
  });
});
