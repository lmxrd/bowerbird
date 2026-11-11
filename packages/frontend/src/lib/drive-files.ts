import type { Drive, Entry, Node } from "organizer-sdk";
import { listChildren } from "organizer-sdk";
import {
  getFileByPathOptions,
  getFileOptions,
  getRootOptions,
  listDrivesOptions,
} from "organizer-sdk/react-query";
import { queryClient } from "@/lib/api";
import type { DriveRef } from "@/lib/drives";

/** A drive with what is needed to find its folders; My drive needs no root. */
export type DriveAt = DriveRef & { root_id?: string | null };

/** The user's drives (My drive first, then the shared ones by name): one query for every page. */
export function drivesOptions() {
  return listDrivesOptions();
}

/** A folder's child by its stored name (as the app's URLs carry it). */
async function childByName(id: string, name: string): Promise<Node | undefined> {
  const wanted = name.normalize("NFC");
  let after: string | undefined;
  do {
    const page = (
      await listChildren({
        throwOnError: true,
        path: { id },
        query: { limit: 1000, ...(after ? { after } : {}) },
      })
    ).data;
    const found = page.items.find((n) => n.name.normalize("NFC") === wanted);
    if (found) return found;
    after = page.next ?? undefined;
  } while (after);
  return undefined;
}

function unavailable(): never {
  throw new Error("This drive's folder is not available.");
}

/**
 * The file or folder at a path of a drive, from its root. My drive's is
 * looked up by the server (`/files/by-path`, which only knows My drive);
 * a shared drive's is walked down from its root.
 */
export async function entryAt(drive: DriveAt, path: string): Promise<Entry> {
  if (drive.kind === "my") {
    return queryClient.fetchQuery(getFileByPathOptions({ query: { path } }));
  }
  let id = drive.root_id ?? unavailable();
  for (const name of path.split("/").filter(Boolean)) {
    const child = await childByName(id, name);
    if (!child) throw new Error("There's nothing at that path.");
    id = child.id;
  }
  return queryClient.fetchQuery(getFileOptions({ path: { id } }));
}

/** A drive's root folder. */
export async function driveRoot(drive: DriveAt): Promise<Node> {
  if (drive.kind === "my") return queryClient.fetchQuery(getRootOptions());
  return queryClient.fetchQuery(getFileOptions({ path: { id: drive.root_id ?? unavailable() } }));
}

/** My drive in the list of drives, once it is there. */
export function myDrive(drives: Drive[] | undefined): Drive | undefined {
  return drives?.find((d) => d.kind === "my");
}
