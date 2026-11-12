import { useState } from "react";
import { useMutation, useQuery } from "@tanstack/react-query";
import { DownloadIcon, Trash2Icon } from "lucide-react";
import { toast } from "sonner";
import type { Mirror, Node, Source, Version } from "organizer-sdk";
import {
  deleteVersionMutation,
  listVersionsOptions,
  restoreMirrorVersionMutation,
  restoreVersionMutation,
} from "organizer-sdk/react-query";
import { ConfirmDialog } from "@/components/confirm-dialog";
import { ListRow } from "@/components/list-row";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Dialog,
  DialogContent,
  DialogDescription,
  DialogFooter,
  DialogHeader,
  DialogTitle,
} from "@/components/ui/dialog";
import { errorMessage, queryClient } from "@/lib/api";
import { formatDateTime, formatSize } from "@/lib/format";
import { decryptedFile } from "@/lib/encryption";
import { mirrorError } from "@/components/mirrors";
import { restoreAction } from "@/lib/mirrors";
import { can } from "@/lib/roles";
import { saveBlob, versionUrl } from "@/lib/transfer";

const sources: Record<Source, string> = { web: "web", api: "an app", webdav: "WebDAV" };

/**
 * A file's earlier contents: download each, restore one (an editor and
 * above) or delete it for good (`mayPurge`: the owner, a shared drive's
 * managers).
 */
export function VersionsDialog({
  node,
  mayPurge,
  mirror,
  onClose,
}: {
  node: Node;
  mayPurge: boolean;
  /**
   * The mirror the file is in (docs/M7D_DESIGN.md §9): a restore copies
   * the version back into the drive mirrored, or it is only downloaded.
   */
  mirror?: Mirror;
  onClose: () => void;
}) {
  const intoDrive = mirror !== undefined && restoreAction(mirror, "file") === "restore";
  const mayRestore = mirror ? intoDrive : can(node.my_role, "edit");
  const restoreMirror = useMutation({
    ...restoreMirrorVersionMutation(),
    onSuccess: () => {
      void queryClient.invalidateQueries();
      toast.success(`Restored into ${mirror?.drive_name}: its current one is kept as a version`);
      onClose();
    },
    onError: (error) => toast.error(mirrorError(error)),
  });
  const versions = useQuery(listVersionsOptions({ path: { id: node.id } }));
  const [deleting, setDeleting] = useState<Version>();
  const changed = (message: string) => () => {
    void queryClient.invalidateQueries();
    toast.success(message);
  };
  const failed = (error: unknown) => toast.error(errorMessage(error));
  const restore = useMutation({
    ...restoreVersionMutation(),
    onSuccess: () => {
      changed("Restored: the current one is kept as a version")();
      onClose();
    },
    onError: failed,
  });
  const remove = useMutation({
    ...deleteVersionMutation(),
    onSuccess: changed("Deleted"),
    onError: failed,
  });
  return (
    <>
      <Dialog open onOpenChange={(open) => !open && onClose()}>
        <DialogContent className="sm:max-w-lg">
          <DialogHeader>
            <DialogTitle>Versions of {node.name}</DialogTitle>
            <DialogDescription>
              {mirror
                ? intoDrive
                  ? `Kept when the file changed in ${mirror.drive_name}. Restoring writes it back there, keeping the current file as a version.`
                  : `Kept when the file changed in ${mirror.drive_name}. Download one to keep it: you can't write to that drive.`
                : "Kept when the file is replaced through Organizer. Restoring keeps the current file as a version."}
            </DialogDescription>
          </DialogHeader>
          <div className="flex flex-col divide-y">
            <ListRow title="Now" detail={formatSize(node.size)}>
              <Badge variant="secondary">Current</Badge>
            </ListRow>
            {versions.isError && (
              <p className="py-2 text-sm text-destructive">{errorMessage(versions.error)}</p>
            )}
            {versions.data?.length === 0 && (
              <p className="py-3 text-sm text-muted-foreground">No earlier versions.</p>
            )}
            {versions.data?.map((version) => (
              <ListRow
                key={version.id}
                title={formatDateTime(version.created_at)}
                detail={`${formatSize(version.size)}${version.source ? ` · replaced via ${sources[version.source]}` : ""}`}
              >
                {node.e2e_root ? (
                  // Encrypted: decrypted here, under the name the user sees.
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label="Download"
                    onClick={() =>
                      decryptedFile(node, versionUrl(node.id, version.id)).then(
                        (blob) => saveBlob(blob, node.name),
                        failed,
                      )
                    }
                  >
                    <DownloadIcon />
                  </Button>
                ) : (
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label="Download"
                    render={<a href={versionUrl(node.id, version.id)} />}
                  >
                    <DownloadIcon />
                  </Button>
                )}
                {mayRestore && (
                  <Button
                    variant="outline"
                    size="sm"
                    disabled={restore.isPending || restoreMirror.isPending}
                    title={mirror ? `Into ${mirror.drive_name}` : undefined}
                    onClick={() =>
                      mirror
                        ? restoreMirror.mutate({ path: { id: mirror.id, version_id: version.id } })
                        : restore.mutate({
                            path: { id: node.id, version_id: version.id },
                            query: { if_version: node.version },
                          })
                    }
                  >
                    Restore
                  </Button>
                )}
                {mayPurge && (
                  <Button
                    variant="ghost"
                    size="icon-sm"
                    aria-label="Delete"
                    onClick={() => setDeleting(version)}
                  >
                    <Trash2Icon />
                  </Button>
                )}
              </ListRow>
            ))}
          </div>
          <DialogFooter>
            <Button variant="outline" onClick={onClose}>
              Close
            </Button>
          </DialogFooter>
        </DialogContent>
      </Dialog>
      {deleting && (
        <ConfirmDialog
          title="Delete this version?"
          description={`The contents of ${formatDateTime(deleting.created_at)} are deleted for good.`}
          action="Delete"
          onConfirm={() => remove.mutate({ path: { id: node.id, version_id: deleting.id } })}
          onClose={() => setDeleting(undefined)}
        />
      )}
    </>
  );
}
