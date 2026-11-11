/**
 * Moving file content: uploads (into a folder, and into the drop box) and
 * download links. The one place the app talks to the API without the
 * generated SDK. Its client validates bodies as JSON-shaped data, which a
 * file is not, and a download is the browser's own navigation. Everything
 * else goes through `organizer-sdk`.
 */

import type { Node } from "organizer-sdk";
import type { DroppedFolders } from "organizer-sdk/types";
import { encryptedUpload } from "@/lib/encryption";
import { type Dropped, tooMany } from "@/lib/folder-drop";

/** Plain uploads (multipart) go up to this; larger files are resumable. */
export const PLAIN_UPLOAD_MAX = 100_000_000;
/** The size of each request of a resumable upload. */
const CHUNK = 8_000_000;

/** The URL that downloads a file (or shows it, with `inline`). */
export function downloadUrl(id: string, inline = false): string {
  return `/api/files/${id}/content${inline ? "?inline=true" : ""}`;
}

/** The URL that downloads a file from a mirror's trash (instead of restoring it into the drive). */
export function mirrorTrashUrl(mirrorId: string, itemId: string): string {
  return `/api/mirrors/${encodeURIComponent(mirrorId)}/trash/${itemId}/content`;
}

/** The URL that downloads a file of a public link (or shows it, with `inline`). */
export function linkDownloadUrl(token: string, id: string, inline = false): string {
  return `/api/s/${encodeURIComponent(token)}/files/${id}/content${inline ? "?inline=true" : ""}`;
}

/** The URL that downloads a folder of a public link as a zip. */
export function linkZipUrl(token: string, id: string): string {
  return `/api/s/${encodeURIComponent(token)}/folders/${id}/zip`;
}

/**
 * Upload a file through a public link (edit or file drop), into a folder
 * of it (the link's own by default). Answers its name there: a file drop
 * numbers a taken one.
 */
export async function uploadLinkFile(
  token: string,
  file: File,
  folderId?: string,
): Promise<string> {
  const form = new FormData();
  form.append("file", file, file.name);
  const query = folderId ? `?folder=${folderId}` : "";
  const response = await fetch(`/api/s/${encodeURIComponent(token)}/files${query}`, {
    method: "POST",
    body: form,
  });
  if (!response.ok) throw await failure(response);
  return ((await response.json()) as { name: string }).name;
}

/** Where an earlier content of a file is downloaded. */
export function versionUrl(id: string, versionId: string): string {
  return `/api/files/${id}/versions/${versionId}/content`;
}

/** The server refused: not something a retry fixes. */
class Refused extends Error {}

/** An answer that is not 2xx, as an Error with the API's message. */
async function failure(response: Response): Promise<Error> {
  try {
    const body = (await response.json()) as { error?: string };
    if (body.error) return new Refused(body.error);
  } catch {
    // Not JSON: the status says enough.
  }
  return new Refused(`The upload failed (${response.status})`);
}

/**
 * Upload a file into a folder: multipart when it is small, resumable
 * (draft-ietf-httpbis-resumable-upload, interop version 6) when it is
 * large.
 */
export async function uploadFile(
  folderId: string,
  file: File,
  onProgress?: (sent: number) => void,
): Promise<void> {
  if (file.size <= PLAIN_UPLOAD_MAX) {
    const form = new FormData();
    form.append("file", file, file.name);
    const response = await fetch(`/api/files/${folderId}/files`, { method: "POST", body: form });
    if (!response.ok) throw await failure(response);
    onProgress?.(file.size);
    return;
  }
  await resumable(new URLSearchParams({ parent_id: folderId, name: file.name }), file, onProgress);
}

/**
 * Upload a file into an encrypted folder (docs/E2E_SPEC.md): its name
 * sealed for the folder and its contents encrypted a segment at a time as
 * they go, never whole in memory: multipart when small, resumable when
 * large, as any upload.
 */
export async function uploadEncryptedFile(
  folder: Pick<Node, "id" | "e2e_root" | "source_id" | "remote">,
  file: File,
  onProgress?: (sent: number) => void,
): Promise<void> {
  const { name, contents } = await encryptedUpload(folder, file);
  if (contents.size <= PLAIN_UPLOAD_MAX) {
    const parts: Blob[] = [];
    for (let at = 0; at < contents.size; at += CHUNK) {
      parts.push(new Blob([await contents.slice(at, Math.min(at + CHUNK, contents.size))]));
    }
    const form = new FormData();
    form.append("file", new Blob(parts), name);
    const response = await fetch(`/api/files/${folder.id}/files`, { method: "POST", body: form });
    if (!response.ok) throw await failure(response);
    onProgress?.(file.size);
    return;
  }
  await resumable(new URLSearchParams({ parent_id: folder.id, name }), contents, onProgress);
}

/**
 * The types a decrypted file may be shown in a tab as: a blob URL is of
 * this origin, so nothing that could run as a page (HTML, SVG, ...) is ever
 * shown, only saved. Undefined: save it.
 */
export function inlineType(name: string): string | undefined {
  const ext = name.toLowerCase().split(".").pop() ?? "";
  const types: Record<string, string> = {
    pdf: "application/pdf",
    png: "image/png",
    jpg: "image/jpeg",
    jpeg: "image/jpeg",
    gif: "image/gif",
    webp: "image/webp",
    txt: "text/plain",
    md: "text/plain",
    mp3: "audio/mpeg",
    mp4: "video/mp4",
  };
  return name.includes(".") ? types[ext] : undefined;
}

/** Save bytes made in the browser as a download named `name`. */
export function saveBlob(blob: Blob, name: string) {
  const a = document.createElement("a");
  a.href = URL.createObjectURL(blob);
  a.download = name;
  a.click();
  setTimeout(() => URL.revokeObjectURL(a.href), 60_000);
}

/** What a resumable upload sends: a file, or one being encrypted. */
type Source = { size: number; slice: (start: number, end: number) => Blob | Promise<BodyInit> };

/**
 * A resumable upload, made with `query` (where it goes): a request that
 * breaks off asks the server for its offset and goes on from there, up to
 * a few times.
 */
async function resumable(
  query: URLSearchParams,
  file: Source,
  onProgress?: (sent: number) => void,
): Promise<void> {
  const created = await fetch(`/api/uploads?${query}`, {
    method: "POST",
    headers: { "Upload-Complete": "?0", "Upload-Draft-Interop-Version": "6" },
  });
  if (!created.ok) throw await failure(created);
  const location = created.headers.get("Location");
  if (!location) throw new Error("The server did not say where the upload is");

  let offset = 0;
  let retries = 0;
  while (true) {
    const end = Math.min(offset + CHUNK, file.size);
    const last = end === file.size;
    try {
      const response = await fetch(location, {
        method: "PATCH",
        headers: {
          "Content-Type": "application/partial-upload",
          "Upload-Offset": String(offset),
          "Upload-Complete": last ? "?1" : "?0",
          "Upload-Draft-Interop-Version": "6",
        },
        body: await file.slice(offset, end),
      });
      if (response.status === 409) {
        // Another offset: the server says which.
        offset = Number(response.headers.get("Upload-Offset") ?? offset);
        continue;
      }
      if (!response.ok) throw await failure(response);
      offset = end;
      retries = 0;
      onProgress?.(offset);
      if (last) return;
    } catch (error) {
      // A broken connection: ask where the upload is, and go on. A refusal
      // is final.
      if (error instanceof Refused || retries++ >= 5) {
        await fetch(location, { method: "DELETE" }).catch(() => {});
        throw error;
      }
      await new Promise((r) => setTimeout(r, 1000 * retries));
      const head = await fetch(location, { method: "HEAD" });
      if (!head.ok) throw error;
      offset = Number(head.headers.get("Upload-Offset") ?? 0);
    }
  }
}

/** Which dropped files go up on their own, resumably: loose ones too large for a multipart drop. */
export function largeLooseFiles(files: Dropped[]): Dropped[] {
  return files.filter(({ file, path }) => file.size > PLAIN_UPLOAD_MAX && !path.includes("/"));
}

/**
 * Drop files into the drop box, at most 1,000 in one drop. A file of a
 * dropped folder goes under its path in it, which tells the server it is
 * part of a folder; those and small files go together as one multipart
 * request (each up to 100 MB). A loose file larger than that goes up on
 * its own, resumably. The note, the read-contents choice and what to do
 * with the folders apply to all.
 */
export async function dropFiles(
  files: Dropped[],
  options: { note?: string; readContents?: boolean; folders?: DroppedFolders } = {},
): Promise<void> {
  const refused = tooMany(files.length);
  if (refused) throw new Error(refused);
  const large = largeLooseFiles(files);
  const rest = files.filter((f) => !large.includes(f));
  for (const { file } of large) {
    const query = new URLSearchParams({ drop_box: "true", name: file.name });
    if (options.note) query.set("note", options.note);
    if (options.readContents !== undefined)
      query.set("read_contents", String(options.readContents));
    await resumable(query, file);
  }
  if (rest.length === 0) return;
  const form = new FormData();
  if (options.note) form.append("note", options.note);
  if (options.readContents !== undefined) {
    form.append("read_contents", String(options.readContents));
  }
  if (options.folders) form.append("folders", options.folders);
  for (const { file, path } of rest) form.append("file", file, path);
  const response = await fetch("/api/drop-box/files", { method: "POST", body: form });
  if (!response.ok) throw await failure(response);
}
