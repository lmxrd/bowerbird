/**
 * Files dropped into the drop box with their path: a file of a dropped
 * folder is sent under its path in it (`Japan trip/tickets/train.txt`), so
 * the server can keep the folder whole or split it.
 */

/** A file to drop, and the name it is sent under. */
export type Dropped = { file: File; path: string };

/** The most files the server takes in one drop. */
export const DROP_MAX_FILES = 1000;

/** Why a drop is refused before it is sent: too many files; else none. */
export function tooMany(count: number): string | undefined {
  if (count <= DROP_MAX_FILES) return undefined;
  return `That is ${count.toLocaleString("en")} files: drop at most ${DROP_MAX_FILES.toLocaleString("en")} at a time`;
}

/**
 * A chosen or pasted file: its path in the folder it was chosen from
 * (`webkitRelativePath`, set by a `webkitdirectory` input), else its name.
 */
export function fromFile(file: File): Dropped {
  return { file, path: file.webkitRelativePath || file.name };
}

/** The folders at the top of a drop, in the order they come. */
export function topFolders(files: Dropped[]): string[] {
  const folders = new Set<string>();
  for (const { path } of files) {
    const slash = path.indexOf("/");
    if (slash > 0) folders.add(path.slice(0, slash));
  }
  return [...folders];
}

/** The files of a dragged item (a file, or a folder read all the way down). */
async function walk(entry: FileSystemEntry, parent: string, out: Dropped[]): Promise<void> {
  const path = parent ? `${parent}/${entry.name}` : entry.name;
  if (entry.isFile) {
    const file = await new Promise<File>((resolve, reject) =>
      (entry as FileSystemFileEntry).file(resolve, reject),
    );
    out.push({ file, path });
  } else if (entry.isDirectory) {
    const reader = (entry as FileSystemDirectoryEntry).createReader();
    // readEntries gives a batch at a time (100 in Chrome): call it until
    // it gives none.
    while (true) {
      const batch = await new Promise<FileSystemEntry[]>((resolve, reject) =>
        reader.readEntries(resolve, reject),
      );
      if (batch.length === 0) break;
      for (const child of batch) await walk(child, path, out);
    }
  }
}

/**
 * The files of dragged items (`DataTransferItem.webkitGetAsEntry()`, taken
 * during the drop event), with their path from the dragged folder.
 */
export async function collectEntries(entries: FileSystemEntry[]): Promise<Dropped[]> {
  const out: Dropped[] = [];
  for (const entry of entries) await walk(entry, "", out);
  return out;
}
