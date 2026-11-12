import { FileIcon, FolderIcon, FolderLockIcon } from "lucide-react";
import type { Kind } from "organizer-sdk";

/**
 * A file's or a folder's icon, at the size of a line of a list; an
 * end-to-end-encrypted folder's has a lock.
 */
export function KindIcon({ kind, encrypted = false }: { kind: Kind; encrypted?: boolean }) {
  const Icon = kind === "folder" ? (encrypted ? FolderLockIcon : FolderIcon) : FileIcon;
  return (
    <Icon
      className="size-4 shrink-0 text-muted-foreground"
      aria-label={encrypted ? "Encrypted" : undefined}
    />
  );
}
