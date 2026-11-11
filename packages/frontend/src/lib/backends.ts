/**
 * Storage backends (docs/M7C_DESIGN.md): the server's disk and S3-compatible
 * storage, global (the superadmin's) or a user's own. The pure logic of
 * Settings → Storage, "Stored on" where drives are made, and what a drive's
 * page says of where it is stored.
 */
import type {
  Backend,
  BackendChange,
  BackendChoice,
  BackendRef,
  Drive,
  MakeDrives,
  NewBackend,
} from "organizer-sdk";
import { limitToGb, parseLimitGb } from "@/lib/users";

/** The server's disk, in the API: it has no row of its own. */
export const DISK_ID = "00000000-0000-0000-0000-000000000000";

/** What a backend's row says of whether its drives can be used. */
export type BackendState = { label: string; tone: "ok" | "error" | "locked" };

/**
 * A backend's badge: Connected, Unreachable (or the reason it fails) and
 * Locked. A user's own private one that answers is "Unlocked".
 */
export function backendState(backend: Pick<Backend, "status" | "private">): BackendState {
  switch (backend.status) {
    case "failing":
      return { label: "Unreachable", tone: "error" };
    case "locked":
      return { label: "Locked", tone: "locked" };
    default:
      return { label: backend.private ? "Unlocked" : "Connected", tone: "ok" };
  }
}

/** Who may make drives on a global backend. */
export const MAKE_DRIVES: readonly { value: MakeDrives; label: string }[] = [
  { value: "superadmin", label: "Superadmin" },
  { value: "team_managers", label: "Team managers" },
  { value: "everyone", label: "Everyone" },
  { value: "teams", label: "Chosen teams" },
];

/** How often a backend's drives are listed for changes made outside Organizer, in minutes. */
export const LIST_EVERY: readonly { value: string; label: string }[] = [
  { value: "15", label: "Every 15 minutes" },
  { value: "60", label: "Every hour" },
  { value: "360", label: "Every 6 hours" },
  { value: "1440", label: "Every day" },
  { value: "0", label: "Never" },
];

/** "Every hour", or "Every 90 minutes" for a value set elsewhere. */
export function listEveryLabel(minutes: number): string {
  return LIST_EVERY.find((o) => o.value === String(minutes))?.label ?? `Every ${minutes} minutes`;
}

/** The options of the "look for changes" select, with a value set elsewhere kept. */
export function listEveryOptions(current: string) {
  return LIST_EVERY.some((o) => o.value === current)
    ? LIST_EVERY
    : [...LIST_EVERY, { value: current, label: listEveryLabel(Number(current)) }];
}

/** "my-bucket/organizer/drives" as its bucket and its prefix. */
export function splitBucket(text: string): { bucket: string; prefix: string } {
  const trimmed = text.trim().replace(/^\/+/, "");
  const at = trimmed.indexOf("/");
  if (at < 0) return { bucket: trimmed, prefix: "" };
  return {
    bucket: trimmed.slice(0, at),
    prefix: trimmed
      .slice(at + 1)
      .replace(/\/+$/, "")
      .trim(),
  };
}

/** A bucket and its prefix as one field shows them. */
export function joinBucket(bucket: string | null, prefix: string | null): string {
  if (!bucket) return "";
  return prefix ? `${bucket}/${prefix}` : bucket;
}

/** The fields of the Add / Edit S3 storage form. */
export type BackendForm = {
  name: string;
  endpoint: string;
  region: string;
  /** Bucket and prefix: "my-bucket/organizer". */
  bucket: string;
  accessKey: string;
  /** Empty: keep the stored one (an edit). */
  secret: string;
  makeDrives: MakeDrives;
  teamIds: string[];
  /** GB, empty for none. */
  limit: string;
  driveLimit: string;
  /** Minutes, as a string ("60"); "0" for never. */
  listEvery: string;
  private: boolean;
};

export function backendForm(backend?: Backend): BackendForm {
  return {
    name: backend?.name ?? "",
    endpoint: backend?.endpoint ?? "",
    region: backend?.region ?? "",
    bucket: backend ? joinBucket(backend.bucket, backend.prefix) : "",
    accessKey: backend?.access_key ?? "",
    secret: "",
    makeDrives: backend?.make_drives ?? "superadmin",
    teamIds: backend?.team_ids ?? [],
    limit: limitToGb(backend?.limit),
    driveLimit: limitToGb(backend?.drive_limit),
    listEvery: String(backend?.list_every_minutes ?? 60),
    private: backend?.private ?? false,
  };
}

/** What is wrong with the form before it is sent, if anything. */
export function formProblem(form: BackendForm, adding: boolean): string | undefined {
  if (!form.name.trim()) return "A name is needed";
  if (!form.endpoint.trim()) return "An endpoint is needed";
  if (!splitBucket(form.bucket).bucket) return "A bucket is needed";
  if (!form.accessKey.trim()) return "An access key is needed";
  if (adding && !form.secret) return "A secret is needed";
  if (parseLimitGb(form.limit) === undefined || parseLimitGb(form.driveLimit) === undefined) {
    return "Expected a number of GB, or nothing for no limit";
  }
  if (form.makeDrives === "teams" && form.teamIds.length === 0) return "Choose at least one team";
  return undefined;
}

/**
 * The body of a new backend (and of a test of unsaved settings). `own`:
 * a user's own storage, which has no "who makes drives". The sealed secret
 * of private credentials is added by the caller.
 */
export function newBackendBody(form: BackendForm, own: boolean): NewBackend {
  const { bucket, prefix } = splitBucket(form.bucket);
  return {
    name: form.name.trim(),
    endpoint: form.endpoint.trim(),
    region: form.region.trim(),
    bucket,
    prefix,
    access_key: form.accessKey.trim(),
    secret: form.secret,
    limit: parseLimitGb(form.limit) ?? null,
    drive_limit: parseLimitGb(form.driveLimit) ?? null,
    list_every_minutes: Number(form.listEvery),
    ...(own
      ? { private: form.private }
      : {
          make_drives: form.makeDrives,
          team_ids: form.makeDrives === "teams" ? form.teamIds : [],
        }),
  };
}

/** What an edit changes: the fields that differ, and a new secret when one was typed. */
export function backendChanges(form: BackendForm, backend: Backend, own: boolean): BackendChange {
  const before = backendForm(backend);
  const now = newBackendBody(form, own);
  const out: BackendChange = {};
  if (form.name.trim() !== before.name) out.name = now.name;
  if (form.endpoint.trim() !== before.endpoint) out.endpoint = now.endpoint;
  if (form.region.trim() !== before.region) out.region = now.region;
  if (form.bucket.trim() !== before.bucket) {
    out.bucket = now.bucket;
    out.prefix = now.prefix;
  }
  if (form.accessKey.trim() !== before.accessKey) out.access_key = now.access_key;
  if (form.secret) out.secret = form.secret;
  if (form.limit.trim() !== before.limit) out.limit = now.limit;
  if (form.driveLimit.trim() !== before.driveLimit) out.drive_limit = now.drive_limit;
  if (form.listEvery !== before.listEvery) out.list_every_minutes = now.list_every_minutes;
  if (!own) {
    if (form.makeDrives !== before.makeDrives) out.make_drives = form.makeDrives;
    const teams = form.makeDrives === "teams" ? form.teamIds : [];
    if ([...teams].sort().join() !== [...before.teamIds].sort().join()) out.team_ids = teams;
  }
  return out;
}

/**
 * The line under a backend's name: its bucket (the endpoint is in its
 * edit dialog), its drives and whether the server backs it up.
 */
export function backendDetail(
  backend: Pick<Backend, "bucket" | "prefix" | "drives" | "backed_up">,
): string {
  const drives = `${backend.drives} ${backend.drives === 1 ? "drive" : "drives"}`;
  return [
    joinBucket(backend.bucket, backend.prefix),
    drives,
    backend.backed_up ? "Backed up" : "Not backed up by Organizer",
  ]
    .filter(Boolean)
    .join(" · ");
}

/** A place a new drive may be stored, for the "Stored on" select. */
export function choiceLabel(choice: Pick<BackendChoice, "name" | "owner" | "available">): string {
  const name = choice.owner ? `${choice.name} (your storage)` : choice.name;
  return choice.available ? name : `${name}: unavailable`;
}

/** The "Stored on" select's options; one that cannot be used now cannot be chosen. */
export function choiceOptions(choices: BackendChoice[] | undefined) {
  return (choices ?? []).map((c) => ({
    value: c.id,
    label: choiceLabel(c),
    disabled: !c.available,
  }));
}

/** What to say under "Stored on" when the server's backups do not cover it. */
export function backupNote(choice: Pick<BackendRef, "backed_up"> | undefined): string | undefined {
  if (!choice || choice.backed_up) return undefined;
  return "Not in the server's backups. Use the provider's versioning.";
}

/** The backend to send: none for the server's disk. */
export function chosenBackend(id: string): string | undefined {
  return id && id !== DISK_ID ? id : undefined;
}

/**
 * The warning on a drive stored on someone's own storage; none for the
 * server's disk or a global backend. `me`: the user's email.
 */
export function ownStorageWarning(drive: Pick<Drive, "backend">, me?: string): string | undefined {
  const owner = drive.backend.owner;
  if (!owner) return undefined;
  if (owner === me) {
    return "Stored on your own storage. If you remove it, this drive goes. The server doesn't back it up.";
  }
  return `Stored on ${owner}'s own storage. If they remove it, this drive goes. The server doesn't back it up.`;
}

/** Where a drive is stored, for its page's line: "Stored on Backblaze". */
export function storedOn(drive: Pick<Drive, "backend">): string {
  return drive.backend.id === DISK_ID ? "Stored on the server" : `Stored on ${drive.backend.name}`;
}

/** "Moving to Backblaze…" while a drive is moved; none otherwise. */
export function movingText(drive: Pick<Drive, "moving_to">): string | undefined {
  return drive.moving_to ? `Moving to ${drive.moving_to.name}…` : undefined;
}

/** Why a drive cannot be used now ("Locked until ana@example.com signs in"); none when it can. */
export function unavailableText(
  drive: Pick<Drive, "available" | "unavailable_reason">,
): string | undefined {
  if (drive.available) return undefined;
  return drive.unavailable_reason ?? "This drive's storage can't be used right now";
}

/** Where a drive may move to: the choices but where it is now and those unavailable. */
export function moveTargets(choices: BackendChoice[] | undefined, drive: Pick<Drive, "backend">) {
  return choiceOptions(choices?.filter((c) => c.id !== drive.backend.id));
}

/** The longest sealed secret the server keeps, in base64url characters. */
export const MAX_SEALED = 4096;
