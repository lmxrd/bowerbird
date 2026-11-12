import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { ArchiveIcon, ChevronLeftIcon } from "lucide-react";
import { toast } from "sonner";
import {
  listSnapshotFilesOptions,
  listSnapshotsOptions,
  restoreFromSnapshotMutation,
} from "organizer-sdk/react-query";
import { EmptyState } from "@/components/empty-state";
import { KindIcon } from "@/components/kind-icon";
import { ListRow } from "@/components/list-row";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { errorMessage, queryClient } from "@/lib/api";
import { formatDateTime, formatSize } from "@/lib/format";

/** `/Taxes/2025` as the backups take it: `Taxes/2025`. */
const inner = (path: string) => path.split("/").filter(Boolean).join("/");

/**
 * A folder as it was in one of the server's backups: pick a backup, go
 * into its folders, and restore a file or folder next to the current one.
 */
export function BackupDialog({ path, onClose }: { path: string; onClose: () => void }) {
  const snapshots = useQuery({ ...listSnapshotsOptions(), retry: false });
  const [chosen, setChosen] = useState<string>();
  const snapshot = chosen ?? snapshots.data?.[0]?.id;
  const [folder, setFolder] = useState(inner(path));
  const files = useQuery({
    ...listSnapshotFilesOptions({ path: { snapshot: snapshot ?? "" }, query: { path: folder } }),
    enabled: !!snapshot,
    retry: false,
  });
  const restore = useMutation({
    ...restoreFromSnapshotMutation(),
    onSuccess: (node) => {
      void queryClient.invalidateQueries({ predicate: (q) => !isBackupQuery(q.queryKey) });
      toast.success(`Restored as ${node.name}`);
    },
    onError: (error) => toast.error(errorMessage(error)),
  });
  const title = folder ? `/${folder}` : "Your files";
  const up = folder.split("/").slice(0, -1).join("/");
  return (
    <Dialog open onOpenChange={(open) => !open && onClose()}>
      <DialogContent className="sm:max-w-xl">
        <DialogHeader>
          <DialogTitle>{title} from a backup</DialogTitle>
          <DialogDescription>
            What you restore comes back next to what is there now, with the backup's date in its
            name.
          </DialogDescription>
        </DialogHeader>
        {snapshots.isError ? (
          <EmptyState icon={ArchiveIcon} title="No backups">
            The server is not backed up yet: a superadmin sets it up in Settings → Backups.
          </EmptyState>
        ) : (
          <>
            <div className="flex items-center gap-2">
              {folder && (
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label="Up a folder"
                  onClick={() => setFolder(up)}
                >
                  <ChevronLeftIcon />
                </Button>
              )}
              <span className="text-sm text-muted-foreground">Backup</span>
              <Select
                items={(snapshots.data ?? []).map((s) => ({
                  value: s.id,
                  label: formatDateTime(s.time),
                }))}
                value={snapshot ?? null}
                onValueChange={(value) => value && setChosen(value)}
              >
                <SelectTrigger className="flex-1">
                  <SelectValue placeholder={snapshots.isPending ? "Loading…" : "Choose"} />
                </SelectTrigger>
                <SelectContent>
                  {snapshots.data?.map((s) => (
                    <SelectItem key={s.id} value={s.id}>
                      {formatDateTime(s.time)}
                    </SelectItem>
                  ))}
                </SelectContent>
              </Select>
            </div>
            <div className="flex max-h-80 flex-col divide-y overflow-y-auto">
              {files.isError && (
                <p className="py-2 text-sm text-destructive">{errorMessage(files.error)}</p>
              )}
              {files.data?.length === 0 && (
                <p className="py-3 text-sm text-muted-foreground">This folder was empty.</p>
              )}
              {files.data?.map((entry) => {
                const entryPath = folder ? `${folder}/${entry.name}` : entry.name;
                return (
                  <ListRow
                    key={entry.name}
                    icon={<KindIcon kind={entry.kind} />}
                    title={
                      entry.kind === "folder" ? (
                        <button
                          type="button"
                          className="truncate font-medium hover:underline"
                          onClick={() => setFolder(entryPath)}
                        >
                          {entry.name}
                        </button>
                      ) : (
                        entry.name
                      )
                    }
                    detail={entry.kind === "file" ? formatSize(entry.size) : undefined}
                  >
                    <Button
                      variant="outline"
                      size="sm"
                      disabled={restore.isPending || !snapshot}
                      onClick={() =>
                        snapshot &&
                        restore.mutate({ path: { snapshot }, body: { path: entryPath } })
                      }
                    >
                      Restore
                    </Button>
                  </ListRow>
                );
              })}
            </div>
          </>
        )}
        <DialogFooter>
          <Button variant="outline" onClick={onClose}>
            Close
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  );
}

/** The backups' queries are slow (the repository is opened): kept on a restore. */
function isBackupQuery(key: readonly unknown[]): boolean {
  const first = key[0] as { _id?: string } | undefined;
  return first?._id === "listSnapshots" || first?._id === "listSnapshotFiles";
}
